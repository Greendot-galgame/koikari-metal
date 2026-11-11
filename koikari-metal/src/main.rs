//! `koikari-metal` — a native macOS KiriKiri/KAG player.
//!
//! The Windows executable and every Steam component are absent by design: the
//! app opens the player's own `data.xp3`/`patch.xp3` read-only, runs the
//! re-implemented KAG scene machine and draws with Metal.

mod debug_capture;
mod transient_notice;

use debug_capture::DebugCapture;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use transient_notice::{NoticeKind, TransientNotice, audio_summary, visible_notice};

use anyhow::{Context, Result, anyhow, bail};
use koikari_metal::assets::AssetStore;
use koikari_metal::audio::AudioEngine;
use koikari_metal::renderer::{
    Renderer, TEXTURE_BUDGET, Viewport, choice_at, choice_rect, contains, message_text_left,
    show_dialogue, title_menu_rect,
};
use koikari_metal::runtime::{AudioEffect, Event, Runtime, Sprite};
use koikari_metal::text;
use koikari_metal::{CANVAS_HEIGHT, CANVAS_WIDTH};

const WINDOW_WIDTH: f64 = 1280.0;
const WINDOW_HEIGHT: f64 = 720.0;

/// Fixed layout in 2560x1440 canvas coordinates.
const MESSAGE_TOP: f32 = CANVAS_HEIGHT - 360.0;
const TEXT_LEFT: f32 = 170.0;
const TEXT_WIDTH: f32 = 2220.0;
const SPEAKER_TOP: f32 = MESSAGE_TOP + 18.0;
const BODY_TOP: f32 = MESSAGE_TOP + 96.0;
const BODY_HEIGHT: f32 = 250.0;

/// Never overwrite legacy saves merely because an extra resource pack appeared.
fn default_save_dir(home: &Path, additional: bool) -> PathBuf {
    let root = home.join("Library/Application Support/KoikariMetal");
    if additional {
        root.join("with-additional")
    } else {
        root
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Screen {
    Title,
    Game,
    History,
    Menu,
}

struct Player {
    assets: Arc<AssetStore>,
    runtime: Runtime,
    renderer: Renderer,
    audio: AudioEngine,
    screen: Screen,
    auto: bool,
    skip: bool,
    hide_message: bool,
    last_step: Instant,
    /// Persistent script, image and rendering diagnostics.
    status: Option<String>,
    /// Retryable save/load failures persist until a successful save/load.
    save_error: Option<String>,
    /// Explicit save/load successes are short, non-error feedback.
    success_notice: TransientNotice,
    /// Non-fatal audio diagnostics never overwrite persistent errors.
    audio_notice: TransientNotice,
    quit: bool,
}

impl Player {
    fn new(root: &Path, texture_budget: usize, base_only: bool) -> Result<Self> {
        let assets = Arc::new(
            (if base_only {
                AssetStore::open_base(root)
            } else {
                AssetStore::open(root)
            })
            .with_context(|| format!("open game archives in {}", root.display()))?,
        );
        let runtime = Runtime::new(assets.clone())?;
        let renderer = Renderer::with_texture_budget(texture_budget)?;
        let audio = AudioEngine::new();
        let mut player = Self {
            assets,
            runtime,
            renderer,
            audio,
            screen: Screen::Title,
            auto: false,
            skip: false,
            hide_message: false,
            last_step: Instant::now(),
            status: None,
            save_error: None,
            success_notice: TransientNotice::default(),
            audio_notice: TransientNotice::default(),
            quit: false,
        };
        // Surface a missing output device once on the title screen, not as a
        // permanent status first discovered when the player starts a sentence.
        player.collect_audio_warning();
        Ok(player)
    }

    /// Reuse a resident texture, or reload it after GPU cache eviction.
    fn image(&mut self, storage: &str) -> Option<(String, u32, u32)> {
        let resolved = match self.assets.resolve(storage) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.status = Some(format!("missing image {storage}: {error}"));
                return None;
            }
        };
        let key = format!("img:{}", koikari_metal::normalize_asset_key(&resolved));
        if let Some((width, height)) = self.renderer.texture_size(&key) {
            return Some((key, width, height));
        }
        let bytes = match self.assets.read(storage) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.status = Some(format!("missing image {storage}: {error}"));
                return None;
            }
        };
        let decoded = match image::load_from_memory(&bytes) {
            Ok(decoded) => decoded.to_rgba8(),
            Err(error) => {
                self.status = Some(format!("cannot decode {storage}: {error}"));
                return None;
            }
        };
        let (width, height) = (decoded.width(), decoded.height());
        let mut pixels = decoded.into_raw();
        // The pipeline blends with premultiplied alpha (`source = one`), while
        // PNG/WebP store straight alpha. Without this, fully transparent pixels
        // keep RGB 255 and paint solid white over the scene.
        for pixel in pixels.chunks_exact_mut(4) {
            let alpha = u32::from(pixel[3]);
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
            }
        }
        if let Some(dump_dir) = std::env::var_os("KOIKARI_DUMP_IMAGES") {
            let dir = PathBuf::from(dump_dir);
            let _ = std::fs::create_dir_all(&dir);
            let name = storage.replace(['/', '\\'], "_");
            let _ = image::save_buffer(
                dir.join(format!("{name}.png")),
                &pixels,
                width,
                height,
                image::ExtendedColorType::Rgba8,
            );
        }
        if let Err(error) = self.renderer.upload_rgba(&key, width, height, &pixels) {
            self.status = Some(format!("cannot upload {storage}: {error}"));
            return None;
        }
        Some((key, width, height))
    }

    /// Rasterise text through CoreText and cache it as a texture.
    fn text_texture(
        &mut self,
        text: &str,
        width: u32,
        height: u32,
        font: f32,
        color: [f32; 4],
        bold: bool,
        centered: bool,
    ) -> Option<(String, u32, u32)> {
        if text.trim().is_empty() {
            return None;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        text.hash(&mut hasher);
        width.hash(&mut hasher);
        height.hash(&mut hasher);
        font.to_bits().hash(&mut hasher);
        for channel in color {
            channel.to_bits().hash(&mut hasher);
        }
        bold.hash(&mut hasher);
        centered.hash(&mut hasher);
        let cache_key = hasher.finish();
        let key = format!("txt:{cache_key:016x}");
        if let Some((width, height)) = self.renderer.texture_size(&key) {
            return Some((key, width, height));
        }
        let bitmap = text::rasterize(text, width, height, font, color, bold, centered)?;
        // CoreText already produced premultiplied RGBA, which matches the
        // renderer's source=One blending. Residency, including text dimensions,
        // has a single owner; no stale side-cache survives a GPU eviction.
        if let Err(error) =
            self.renderer
                .upload_rgba(&key, bitmap.width, bitmap.height, &bitmap.rgba)
        {
            self.status = Some(format!("text upload failed: {error}"));
            return None;
        }
        Some((key, bitmap.width, bitmap.height))
    }

    fn draw_image(&mut self, storage: &str, rect: [f32; 4], opacity: f32) {
        if let Some((key, _, _)) = self.image(storage) {
            self.renderer.draw(Some(&key), rect, opacity);
        }
    }

    fn draw_text_box(
        &mut self,
        text: &str,
        rect: [f32; 4],
        font: f32,
        color: [f32; 4],
        bold: bool,
        centered: bool,
        opacity: f32,
    ) {
        let width = rect[2].round().max(1.0) as u32;
        let height = rect[3].round().max(1.0) as u32;
        if let Some((key, _, _)) =
            self.text_texture(text, width, height, font, color, bold, centered)
        {
            self.renderer.draw(Some(&key), rect, opacity);
        }
    }

    /// A solid colour rectangle. The renderer only knows how to draw textured
    /// quads, so each distinct colour becomes a cached 1x1 texture with
    /// premultiplied alpha.
    fn plate(&mut self, rect: [f32; 4], color: [f32; 4]) {
        let alpha = color[3];
        if alpha <= 0.0 || rect[2] <= 0.0 || rect[3] <= 0.0 {
            return;
        }
        let channels: [u8; 4] = [
            (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        ];
        let key = format!(
            "plate:{:02x}{:02x}{:02x}{:02x}",
            channels[0], channels[1], channels[2], channels[3]
        );
        if !self.renderer.has_texture(&key) {
            let a = u32::from(channels[3]);
            let pixel = [
                ((u32::from(channels[0]) * a + 127) / 255) as u8,
                ((u32::from(channels[1]) * a + 127) / 255) as u8,
                ((u32::from(channels[2]) * a + 127) / 255) as u8,
                channels[3],
            ];
            if let Err(error) = self.renderer.upload_rgba(&key, 1, 1, &pixel) {
                self.status = Some(format!("plate upload failed: {error}"));
                return;
            }
        }
        self.renderer.draw(Some(&key), rect, 1.0);
    }

    fn report_audio_notice(&mut self, diagnostic: String) {
        if self.audio_notice.show(diagnostic.clone(), Instant::now()) {
            // Keep the full causal chain in the log. Only the UI is shortened;
            // identical polling failures cannot keep a notice alive forever.
            eprintln!(
                "[{}:{}] {diagnostic}",
                self.runtime.state.scenario, self.runtime.state.source_line
            );
        }
    }

    fn collect_audio_warning(&mut self) {
        if let Some(warning) = self.audio.take_warning() {
            self.report_audio_notice(format!("audio: {warning}"));
        }
    }

    fn apply_effects(&mut self) {
        for effect in self.runtime.take_effects() {
            let result = match effect {
                AudioEffect::Bgm { storage, looping } => {
                    self.audio.play_bgm(&self.assets, &storage, looping)
                }
                AudioEffect::Voice { storage } => self.audio.play_voice(&self.assets, &storage),
                AudioEffect::Se {
                    storage,
                    looping,
                    channel,
                } => self.audio.play_se(&self.assets, &storage, looping, channel),
                AudioEffect::StopAll => {
                    self.audio.stop_all();
                    Ok(())
                }
                AudioEffect::StopBgm => {
                    self.audio.stop_bgm();
                    Ok(())
                }
                AudioEffect::StopVoice => {
                    self.audio.stop_voice();
                    Ok(())
                }
                AudioEffect::StopSe { channel } => {
                    self.audio.stop_se(channel);
                    Ok(())
                }
            };
            if let Err(error) = result {
                self.report_audio_notice(format!("audio: {error:#}"));
            }
        }
        self.collect_audio_warning();
    }

    fn advance(&mut self) {
        if self.screen != Screen::Game {
            return;
        }
        if !self.runtime.state.choices.is_empty() {
            return;
        }
        match self.runtime.advance() {
            Ok(_) => {
                self.status = None;
                self.audio_notice.clear();
                self.apply_effects();
                // VM blocking is read directly from state by the compositor.
            }
            Err(error) => self.status = Some(format!("advance failed: {error}")),
        }
        self.last_step = Instant::now();
    }

    fn choose(&mut self, index: usize) {
        if index >= self.runtime.state.choices.len() {
            return;
        }
        match self.runtime.choose(index) {
            Ok(_) => {
                self.status = None;
                self.audio_notice.clear();
                self.apply_effects();
                self.last_step = Instant::now();
            }
            Err(error) => self.status = Some(format!("choice failed: {error}")),
        }
    }

    fn start_game(&mut self) {
        match self.runtime.new_game() {
            Ok(_) => {
                self.audio_notice.clear();
                self.apply_effects();
                self.screen = Screen::Game;
                self.status = None;
                self.save_error = None;
                self.success_notice.clear();
                self.last_step = Instant::now();
            }
            Err(error) => self.status = Some(format!("cannot start: {error}")),
        }
    }

    fn save_dir(additional: bool) -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os("KOIKARI_SAVE_DIR") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("create save directory {}", dir.display()))?;
            return Ok(dir);
        }
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
        let dir = default_save_dir(Path::new(&home), additional);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create save directory {}", dir.display()))?;
        Ok(dir)
    }

    fn storage_feedback(
        &mut self,
        result: Result<()>,
        success: &str,
        failure: &str,
        now: Instant,
    ) -> bool {
        match result {
            Ok(()) => {
                self.save_error = None;
                self.success_notice.show_success(success.to_owned(), now);
                true
            }
            Err(error) => {
                self.success_notice.clear();
                self.save_error = Some(format!("{failure}: {error:#}"));
                false
            }
        }
    }

    fn quick_save(&mut self) {
        let additional = self.assets.has_archive("adult.xp3");
        self.quick_save_with_directory(|| Self::save_dir(additional));
    }

    fn quick_save_with_directory(&mut self, directory: impl FnOnce() -> Result<PathBuf>) {
        // Ignore F5 on the title screen, before resolving or touching a save path.
        // An unstarted/empty scene must never replace an existing progress file.
        if self.screen == Screen::Title || !self.runtime.is_started() {
            return;
        }
        let snapshot = self.runtime.snapshot();
        let result = directory().and_then(|dir| {
            koikari_metal::save::write_snapshot(dir.join("quicksave.json"), &snapshot)
        });
        self.storage_feedback(result, "已快速存档 (F9 读取)", "存档失败", Instant::now());
    }

    fn quick_load(&mut self) {
        self.quick_load_from_directory(Self::save_dir(self.assets.has_archive("adult.xp3")));
    }

    fn quick_load_from_directory(&mut self, directory: Result<PathBuf>) {
        let result = directory.and_then(|dir| {
            let snapshot = koikari_metal::save::read_snapshot(dir.join("quicksave.json"))?;
            self.runtime.restore(snapshot)
        });
        if self.storage_feedback(result, "已读取快速存档", "读档失败", Instant::now()) {
            self.audio_notice.clear();
            self.apply_effects();
            self.screen = Screen::Game;
            self.status = None;
            self.last_step = Instant::now();
        }
    }

    fn compose_title(&mut self) {
        self.draw_image(
            "ui/title/TI_base_0.webp",
            [0.0, 0.0, CANVAS_WIDTH, CANVAS_HEIGHT],
            1.0,
        );
        let logo_width = 900.0;
        self.draw_image(
            "ui/title/TI_タイトルロゴ.webp",
            [(CANVAS_WIDTH - logo_width) * 0.5, 120.0, logo_width, 486.0],
            1.0,
        );
        let items = ["开始游戏", "读取快速存档", "退出"];
        for (index, label) in items.iter().enumerate() {
            let rect = title_menu_rect(index, (CANVAS_WIDTH, CANVAS_HEIGHT));
            self.plate(rect, [0.05, 0.07, 0.16, 0.78]);
            self.draw_text_box(label, rect, 56.0, [0.97, 0.97, 1.0, 1.0], true, true, 1.0);
        }
        self.plate(
            [0.0, CANVAS_HEIGHT - 84.0, CANVAS_WIDTH, 84.0],
            [0.03, 0.04, 0.10, 0.55],
        );
        self.draw_text_box(
            "Rust + Metal 原生重制 · 不含 Steam 组件",
            [0.0, CANVAS_HEIGHT - 70.0, CANVAS_WIDTH, 60.0],
            34.0,
            [0.75, 0.78, 0.9, 1.0],
            false,
            true,
            0.9,
        );
    }

    /// Keep body/expression/blush as one ordered group at the same origin.
    /// Return its occupied bounds for foreground portrait/text layout.
    fn draw_sprite(&mut self, sprite: &Sprite) -> Option<[f32; 4]> {
        if sprite.opacity <= 0.0 || sprite.scale <= 0.0 {
            return None;
        }
        let mut bounds: Option<[f32; 4]> = None;
        let skip_overlays = std::env::var_os("KOIKARI_NO_OVERLAYS").is_some();
        for storage in std::iter::once(&sprite.storage)
            .chain(sprite.overlays.iter().filter(|_| !skip_overlays))
        {
            if let Some((key, width, height)) = self.image(storage) {
                let rect = [
                    sprite.x,
                    sprite.y,
                    width as f32 * sprite.scale,
                    height as f32 * sprite.scale,
                ];
                self.renderer.draw(Some(&key), rect, sprite.opacity);
                bounds = Some(match bounds {
                    Some(previous) => [
                        rect[0],
                        rect[1],
                        previous[2].max(rect[2]),
                        previous[3].max(rect[3]),
                    ],
                    None => rect,
                });
            }
        }
        bounds
    }

    fn compose_game(&mut self) {
        let background = self.runtime.state.background.clone();
        if let Some(background) = background {
            self.draw_image(&background, [0.0, 0.0, CANVAS_WIDTH, CANVAS_HEIGHT], 1.0);
        } else {
            self.plate(
                [0.0, 0.0, CANVAS_WIDTH, CANVAS_HEIGHT],
                [0.0, 0.0, 0.0, 1.0],
            );
        }

        let mut sprites = self.runtime.state.sprites.clone();
        sprites.sort_by_key(|sprite| (sprite.z, sprite.id));
        for sprite in sprites.iter().filter(|sprite| !sprite.portrait) {
            self.draw_sprite(sprite);
        }

        // Scene sprites sit behind the message frame. The authored face-window
        // portrait sits above it, but below the speaker and dialogue text.
        let has_line =
            !self.runtime.state.text.is_empty() || !self.runtime.state.speaker.is_empty();
        if show_dialogue(
            self.runtime.state.message_visible,
            self.hide_message,
            has_line,
            self.runtime.state.choices.len(),
        ) {
            self.draw_image(
                "ui/ms/MS_mainWindowFrame_0.webp",
                [0.0, MESSAGE_TOP, CANVAS_WIDTH, 360.0],
                1.0,
            );
            let mut portraits = Vec::new();
            for sprite in sprites.iter().filter(|sprite| sprite.portrait) {
                if let Some(bounds) = self.draw_sprite(sprite) {
                    portraits.push(bounds);
                }
            }
            let text_left =
                message_text_left(TEXT_LEFT, TEXT_LEFT + TEXT_WIDTH, MESSAGE_TOP, &portraits);
            let text_width = TEXT_LEFT + TEXT_WIDTH - text_left;
            let speaker = self.runtime.state.speaker.clone();
            if !speaker.is_empty() {
                self.draw_text_box(
                    &speaker,
                    [text_left, SPEAKER_TOP, 900.0_f32.min(text_width), 70.0],
                    48.0,
                    [1.0, 0.84, 0.48, 1.0],
                    true,
                    false,
                    1.0,
                );
            }
            let body = self.runtime.state.text.clone();
            let font = self.runtime.state.font_size as f32;
            self.draw_text_box(
                &body,
                [text_left, BODY_TOP, text_width, BODY_HEIGHT],
                font,
                [0.95, 0.96, 1.0, 1.0],
                false,
                false,
                1.0,
            );
        }

        let choices = self.runtime.state.choices.clone();
        for (index, choice) in choices.iter().enumerate() {
            let Some(rect) = choice_rect(index, choices.len(), (CANVAS_WIDTH, CANVAS_HEIGHT))
            else {
                continue;
            };
            self.plate(rect, [0.06, 0.08, 0.20, 0.88]);
            self.draw_text_box(
                &format!("{}.  {}", index + 1, choice.text),
                [rect[0] + 60.0, rect[1], rect[2] - 120.0, rect[3]],
                54.0_f32.min(rect[3] * 0.55),
                [1.0, 1.0, 1.0, 1.0],
                false,
                false,
                1.0,
            );
        }
    }

    fn compose_history(&mut self) {
        self.plate(
            [0.0, 0.0, CANVAS_WIDTH, CANVAS_HEIGHT],
            [0.02, 0.03, 0.06, 0.94],
        );
        let history = self.runtime.history.clone();
        let start = history.len().saturating_sub(6);
        for (row, entry) in history[start..].iter().enumerate() {
            let top = 120.0 + row as f32 * 200.0;
            let line = if entry.speaker.is_empty() {
                entry.text.clone()
            } else {
                format!("{}：{}", entry.speaker, entry.text)
            };
            self.draw_text_box(
                &line,
                [180.0, top, 2200.0, 190.0],
                40.0,
                [0.92, 0.94, 1.0, 1.0],
                false,
                false,
                1.0,
            );
        }
    }

    fn compose_menu(&mut self) {
        self.plate(
            [0.0, 0.0, CANVAS_WIDTH, CANVAS_HEIGHT],
            [0.02, 0.03, 0.06, 0.8],
        );
        let lines = [
            "菜单",
            "空格 / 回车 / 点击：推进对话",
            "数字键：选择分支",
            "Ctrl：快进    A：自动    B：回忆    H：隐藏文本",
            "F5：快速存档    F9：快速读取",
            "Esc：关闭菜单",
        ];
        for (row, line) in lines.iter().enumerate() {
            self.draw_text_box(
                line,
                [340.0, 360.0 + row as f32 * 120.0, 1880.0, 110.0],
                if row == 0 { 64.0 } else { 44.0 },
                [0.95, 0.96, 1.0, 1.0],
                row == 0,
                row == 0,
                1.0,
            );
        }
    }

    fn persistent_diagnostic(&self) -> Option<&str> {
        // A loaded/selected VM can return Ok(Blocked). Never infer success from
        // Result alone, or hide the live diagnostic behind success feedback.
        self.save_error
            .as_deref()
            .or(self.runtime.state.diagnostic.as_deref())
            .or(self.status.as_deref())
    }

    fn compose(&mut self, width: f32, height: f32) {
        self.renderer.begin_frame(width, height);
        match self.screen {
            Screen::Title => self.compose_title(),
            Screen::Game => self.compose_game(),
            Screen::History => {
                self.compose_game();
                self.compose_history();
            }
            Screen::Menu => {
                self.compose_game();
                self.compose_menu();
            }
        }
        let notice = visible_notice(
            self.persistent_diagnostic(),
            &self.audio_notice,
            &self.success_notice,
            Instant::now(),
        )
        .map(|(kind, message)| (kind, message.to_owned()));
        if let Some((kind, message)) = notice {
            let status = if kind == NoticeKind::Warning {
                audio_summary(&message)
            } else {
                message
            };
            let rect = [60.0, 60.0, 2200.0, 90.0];
            self.plate(rect, kind.background());
            let text_color = match kind {
                NoticeKind::Success => [0.90, 1.0, 0.96, 1.0],
                NoticeKind::Warning => [1.0, 0.96, 0.82, 1.0],
                NoticeKind::Error => [1.0, 0.92, 0.92, 1.0],
            };
            self.draw_text_box(&status, rect, 36.0, text_color, false, false, 1.0);
        }
        if self.runtime.state.ended && self.screen == Screen::Game {
            self.draw_text_box(
                "— 本段落结束 —",
                [0.0, 300.0, CANVAS_WIDTH, 120.0],
                64.0,
                [1.0, 0.9, 0.6, 1.0],
                true,
                true,
                1.0,
            );
        }
    }

    /// Render every replayed stop to exercise texture residency, not just the
    /// final state. Intermediate frames are read back but never written to disk.
    fn debug_event(&mut self, event: Event, replay: bool, size: (u32, u32)) -> Result<()> {
        if event == Event::Blocked {
            bail!(
                "scenario replay blocked at {}:{}: {}",
                self.runtime.state.scenario,
                self.runtime.state.source_line,
                self.runtime
                    .state
                    .diagnostic
                    .as_deref()
                    .unwrap_or("unknown instruction")
            );
        }
        // Debug replay is silent and must not accumulate thousands of deferred
        // audio commands while advancing without the normal interactive loop.
        self.runtime.take_effects();
        if replay {
            self.compose(size.0 as f32, size.1 as f32);
            self.renderer
                .render_offscreen(size.0, size.1, [0.0, 0.0, 0.0, 1.0])?;
        }
        Ok(())
    }

    fn debug_choice(
        &mut self,
        choice: &mut Option<usize>,
        replay: bool,
        size: (u32, u32),
    ) -> Result<bool> {
        if self.runtime.state.choices.is_empty() {
            return Ok(false);
        }
        let Some(index) = choice.take() else {
            return Ok(false);
        };
        let event = self
            .runtime
            .choose(index)
            .with_context(|| format!("debug --choice {}", index + 1))?;
        self.debug_event(event, replay, size)?;
        Ok(true)
    }

    fn prepare_debug(&mut self, options: &DebugCapture, size: (u32, u32)) -> Result<()> {
        self.screen = Screen::Game;
        let event = if let Some(scenario) = &options.scenario {
            self.runtime.start_at(scenario, options.label.as_deref())?
        } else {
            self.runtime.new_game()?
        };
        self.debug_event(event, options.replay, size)?;
        let mut choice = options.choice;
        if let Some(line) = options.line {
            let scenario = self.runtime.state.scenario.clone();
            let mut steps = 0;
            while self.runtime.state.source_line < line {
                if self.runtime.state.scenario != scenario || self.runtime.state.ended {
                    bail!(
                        "cannot reach --line {line} in {scenario}: stopped at {}:{}",
                        self.runtime.state.scenario,
                        self.runtime.state.source_line
                    );
                }
                if !self.runtime.state.choices.is_empty() {
                    if !self.debug_choice(&mut choice, options.replay, size)? {
                        bail!(
                            "--line {line} encounters a choice first; supply --choice <1-based index>"
                        );
                    }
                } else {
                    let event = self.runtime.advance()?;
                    self.debug_event(event, options.replay, size)?;
                }
                steps += 1;
                if steps >= 100_000 {
                    bail!("--line {line} replay exceeded 100000 stops");
                }
            }
            if self.runtime.state.scenario != scenario {
                bail!(
                    "--line {line} left {scenario} and reached {} instead",
                    self.runtime.state.scenario
                );
            }
        }
        for _ in 0..options.advance {
            if self.runtime.state.ended {
                break;
            }
            if !self.runtime.state.choices.is_empty() {
                if self.debug_choice(&mut choice, options.replay, size)? {
                    continue;
                }
                break;
            }
            let event = self.runtime.advance()?;
            self.debug_event(event, options.replay, size)?;
        }
        // Also support starting directly at a choice, or ending the requested
        // advances exactly at one. A --choice is consumed only once.
        self.debug_choice(&mut choice, options.replay, size)?;
        if choice.is_some() {
            bail!("--choice was requested but no choice was reached; increase --advance");
        }
        Ok(())
    }

    /// Replay through the story without a window and write the final PNG.
    fn capture(&mut self, path: &Path, options: &DebugCapture, size: (u32, u32)) -> Result<()> {
        self.prepare_debug(options, size)?;
        self.compose(size.0 as f32, size.1 as f32);
        let pixels = self
            .renderer
            .render_offscreen(size.0, size.1, [0.0, 0.0, 0.0, 1.0])?;
        image::save_buffer(
            path,
            &pixels,
            size.0,
            size.1,
            image::ExtendedColorType::Rgba8,
        )
        .with_context(|| format!("write {}", path.display()))?;
        println!(
            "captured {}x{} after {} advances -> {}",
            size.0,
            size.1,
            options.advance,
            path.display()
        );
        for sprite in &self.runtime.state.sprites {
            println!(
                "  sprite id={} storage={} overlays={:?} x={} y={} z={} opacity={} portrait={}",
                sprite.id,
                sprite.storage,
                sprite.overlays,
                sprite.x,
                sprite.y,
                sprite.z,
                sprite.opacity,
                sprite.portrait
            );
        }
        println!(
            "background={:?} text={:?}",
            self.runtime.state.background, self.runtime.state.text
        );
        for (index, choice) in self.runtime.state.choices.iter().enumerate() {
            println!("  choice {}: {:?}", index + 1, choice.text);
        }
        println!(
            "scenario={} line={} speaker={:?} sprites={} choices={} bgm={:?}",
            self.runtime.state.scenario,
            self.runtime.state.source_line,
            self.runtime.state.speaker,
            self.runtime.state.sprites.len(),
            self.runtime.state.choices.len(),
            self.runtime.state.bgm
        );
        Ok(())
    }
}

struct App {
    player: Player,
    window: Option<Arc<winit::window::Window>>,
    layer: Option<metal::MetalLayer>,
    cursor: (f64, f64),
    window_shot_done: bool,
}

impl App {
    fn new(player: Player) -> Self {
        Self {
            player,
            window: None,
            layer: None,
            cursor: (0.0, 0.0),
            window_shot_done: false,
        }
    }

    fn attach_layer(&mut self) -> Result<()> {
        use objc::runtime::{BOOL, Object, YES};
        use objc::{class, msg_send, sel, sel_impl};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let window = self
            .window
            .as_ref()
            .ok_or_else(|| anyhow!("window missing"))?;
        let handle = window.window_handle()?.as_raw();
        let RawWindowHandle::AppKit(appkit) = handle else {
            bail!("expected an AppKit window handle");
        };
        let view = appkit.ns_view.as_ptr() as *mut Object;
        let layer = metal::MetalLayer::new();
        layer.set_device(self.player.renderer.device());
        layer.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        layer.set_contents_scale(window.scale_factor());
        layer.set_display_sync_enabled(true);
        layer.set_framebuffer_only(false);
        let object = (&*layer) as *const metal::MetalLayerRef as *mut Object;
        unsafe {
            let wants: BOOL = YES;
            let _: () = msg_send![view, setWantsLayer: wants];
            let _: () = msg_send![view, setLayer: object];
            let _: () = msg_send![class!(NSApplication), sharedApplication];
        }
        self.layer = Some(layer);
        Ok(())
    }

    fn draw(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let Some(layer) = self.layer.as_ref() else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        // Winit's inner_size and CursorMoved are already physical pixels.
        // contents_scale describes AppKit points; multiplying inner_size again
        // renders a 2x Retina window at 4x area and breaks pointer alignment.
        layer.set_contents_scale(window.scale_factor());
        layer.set_drawable_size(core_graphics_types::geometry::CGSize::new(
            size.width as f64,
            size.height as f64,
        ));
        let Some(drawable) = layer.next_drawable() else {
            return;
        };
        self.player.compose(size.width as f32, size.height as f32);
        let shot = if self.window_shot_done {
            None
        } else {
            std::env::var_os("KOIKARI_WINDOW_SHOT").map(PathBuf::from)
        };
        let result = match &shot {
            Some(path) => {
                self.window_shot_done = true;
                self.player
                    .renderer
                    .present_capture(drawable, [0.0, 0.0, 0.0, 1.0], path)
            }
            None => self.player.renderer.present(drawable, [0.0, 0.0, 0.0, 1.0]),
        };
        if let Err(error) = result {
            self.player.status = Some(format!("present failed: {error}"));
        }
        self.player.audio.poll(&self.player.assets);
        self.player.collect_audio_warning();
    }

    fn canvas_pointer(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let size = self.window.as_ref()?.inner_size();
        if size.width == 0 || size.height == 0 {
            return None;
        }
        Viewport::new(
            (CANVAS_WIDTH, CANVAS_HEIGHT),
            (size.width as f32, size.height as f32),
        )
        .surface_to_canvas(x as f32, y as f32)
    }

    fn is_inside_choice(&self, x: f64, y: f64) -> Option<usize> {
        choice_at(
            self.canvas_pointer(x, y)?,
            self.player.runtime.state.choices.len(),
            (CANVAS_WIDTH, CANVAS_HEIGHT),
        )
    }

    fn title_index(&self, x: f64, y: f64) -> Option<usize> {
        let point = self.canvas_pointer(x, y)?;
        (0..3).find(|&index| contains(title_menu_rect(index, (CANVAS_WIDTH, CANVAS_HEIGHT)), point))
    }

    fn title_action(&mut self, index: usize) {
        match index {
            0 => self.player.start_game(),
            1 => self.player.quick_load(),
            2 => self.player.quit = true,
            _ => {}
        }
    }

    fn activate(&mut self) {
        if self.canvas_pointer(self.cursor.0, self.cursor.1).is_none() {
            return;
        }
        match self.player.screen {
            Screen::Title => {
                if let Some(index) = self.title_index(self.cursor.0, self.cursor.1) {
                    self.title_action(index);
                }
            }
            Screen::Game => {
                if let Some(index) = self.is_inside_choice(self.cursor.0, self.cursor.1) {
                    self.player.choose(index);
                } else {
                    self.player.advance();
                }
            }
            Screen::History | Screen::Menu => self.player.screen = Screen::Game,
        }
    }

    fn gameplay_shortcuts_enabled(&self) -> bool {
        self.player.screen != Screen::Title && self.player.runtime.is_started()
    }

    fn key(&mut self, key: &winit::keyboard::Key, pressed: bool) {
        use winit::keyboard::{Key, NamedKey};
        if !pressed {
            return;
        }
        match key {
            Key::Named(NamedKey::Space)
            | Key::Named(NamedKey::Enter)
            | Key::Named(NamedKey::ArrowRight) => match self.player.screen {
                Screen::Title => self.player.start_game(),
                Screen::Game => self.player.advance(),
                Screen::History | Screen::Menu => self.player.screen = Screen::Game,
            },
            Key::Named(NamedKey::Escape) => {
                self.player.screen = match self.player.screen {
                    Screen::Menu | Screen::History => Screen::Game,
                    Screen::Game => Screen::Menu,
                    Screen::Title => Screen::Title,
                }
            }
            Key::Named(NamedKey::F5) => self.player.quick_save(),
            Key::Named(NamedKey::F9) => self.player.quick_load(),
            Key::Character(text) => match text.as_str() {
                "a" | "A" if self.gameplay_shortcuts_enabled() => {
                    self.player.auto = !self.player.auto
                }
                "h" | "H" if self.gameplay_shortcuts_enabled() => {
                    self.player.hide_message = !self.player.hide_message
                }
                "b" | "B" if self.gameplay_shortcuts_enabled() => {
                    self.player.screen = if self.player.screen == Screen::History {
                        Screen::Game
                    } else {
                        Screen::History
                    }
                }
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                    let index = text.as_bytes()[0] as usize - '1' as usize;
                    if self.player.screen == Screen::Title {
                        self.title_action(index);
                    } else if self.player.screen == Screen::Game {
                        self.player.choose(index);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Auto/skip pacing; returns the delay until the next automatic step.
    fn tick(&mut self) -> Option<Duration> {
        if self.player.screen != Screen::Game {
            return None;
        }
        if self.player.runtime.state.diagnostic.is_some()
            || !self.player.runtime.state.choices.is_empty()
            || self.player.runtime.state.ended
        {
            return None;
        }
        if self.player.skip {
            self.player.advance();
            return Some(Duration::from_millis(15));
        }
        if self.player.auto && self.player.last_step.elapsed() > Duration::from_millis(1600) {
            self.player.advance();
            return Some(Duration::from_millis(200));
        }
        if self.player.auto {
            return Some(Duration::from_millis(100));
        }
        None
    }
}

impl winit::application::ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        use winit::window::Window;
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("恋爱，我就借走咯 — Rust + Metal")
            .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT))
            .with_min_inner_size(winit::dpi::LogicalSize::new(960.0, 540.0));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                self.window = Some(Arc::new(window));
                if let Err(error) = self.attach_layer() {
                    eprintln!("cannot attach Metal layer: {error}");
                    event_loop.exit();
                    return;
                }
                text::activate_application();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            Err(error) => {
                eprintln!("cannot create window: {error}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                self.draw();
            }
            WindowEvent::Focused(false) => {
                self.player.skip = false;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.activate();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    use winit::keyboard::Key;
                    if let Key::Named(winit::keyboard::NamedKey::Control) = event.logical_key {
                        self.player.skip = true;
                    }
                }
                let pressed = event.state == ElementState::Pressed;
                self.key(&event.logical_key, pressed);
                if !pressed {
                    if let winit::keyboard::Key::Named(winit::keyboard::NamedKey::Control) =
                        event.logical_key
                    {
                        self.player.skip = false;
                    }
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let delay = self.tick();
        if let Some(delay) = delay {
            std::thread::sleep(delay.min(Duration::from_millis(16)));
        }
        if self.player.quit {
            event_loop.exit();
            return;
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

fn resolve_root(args: &[String]) -> Result<PathBuf> {
    if let Some(index) = args.iter().position(|value| value == "--root") {
        let root = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("--root needs a directory"))?;
        return Ok(PathBuf::from(root));
    }
    if let Some(root) = std::env::var_os("KOIKARI_GAME_ROOT") {
        return Ok(PathBuf::from(root));
    }
    let current = std::env::current_dir()?;
    if current.join("data.xp3").is_file() {
        return Ok(current);
    }
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1).take(4) {
            if ancestor.join("data.xp3").is_file() {
                return Ok(ancestor.to_path_buf());
            }
        }
    }
    bail!("cannot find data.xp3; run from the game directory or pass --root <game directory>");
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|value| value == "--help" || value == "-h") {
        println!("koikari-metal — native macOS Rust + Metal player\n");
        println!("usage: koikari-metal [--root <game dir>] [--capture <png> [--size WxH]]");
        println!(
            "       [--scenario <storage> [--label <label>] [--line N]] [--advance N] [--choice N]"
        );
        println!("       [--capture-replay] [--texture-budget-mib N] [--title]");
        println!(
            "--line replays to the first dialogue at/after that source line, without skipping scene setup."
        );
        println!("--choice is one-based, applied once to the first encountered choice.");
        println!("--base-only ignores adult.xp3 and keeps the original resource/save profile.");
        println!(
            "--capture-replay renders every replay stop (no intermediate PNGs) to exercise cache eviction."
        );
        println!("\ncontrols: space/enter/click advance · 1-9 choose · esc menu · b history");
        println!("          f5 quick save · f9 quick load · a auto · ctrl skip · h hide text");
        return Ok(());
    }
    let options = DebugCapture::parse(&args, TEXTURE_BUDGET).map_err(|error| anyhow!(error))?;
    let root = resolve_root(&args)?;
    let mut player = Player::new(&root, options.texture_budget, options.base_only)?;
    println!(
        "assets: {} entries from {} archive(s), {}",
        player.assets.stats().entry_count,
        player.assets.stats().archive_count,
        player.assets.root().display()
    );
    for archive in player.assets.archives() {
        println!(
            "  archive: {} ({} entries)",
            archive.path().file_name().unwrap().to_string_lossy(),
            archive.stats().entry_count
        );
    }
    if player.assets.has_archive("adult.xp3") {
        println!(
            "additional archive loaded; saves use the separate with-additional profile (use --base-only for legacy saves)"
        );
    }
    if !player.audio.available() {
        println!("audio: unavailable (running silently)");
    }

    if let Some(index) = args.iter().position(|value| value == "--preview-texture") {
        let storage = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("--preview-texture needs an asset name"))?;
        let path = args
            .iter()
            .position(|value| value == "--capture")
            .and_then(|i| args.get(i + 1))
            .ok_or_else(|| anyhow!("--preview-texture also needs --capture <png>"))?;
        let (width, height) = (CANVAS_WIDTH as u32, CANVAS_HEIGHT as u32);
        player.renderer.begin_frame(width as f32, height as f32);
        player
            .renderer
            .draw(None, [0.0, 0.0, width as f32, height as f32], 1.0);
        if let Some((key, w, h)) = player.image(storage) {
            let scale = (width as f32 / w as f32).min(height as f32 / h as f32);
            player.renderer.draw(
                Some(&key),
                [0.0, 0.0, w as f32 * scale, h as f32 * scale],
                1.0,
            );
            println!("preview {storage} texture {w}x{h} scale {scale:.3}");
        }
        let pixels = player
            .renderer
            .render_offscreen(width, height, [1.0, 0.0, 1.0, 1.0])?;
        image::save_buffer(
            path,
            &pixels,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?;
        println!("wrote {}", path);
        return Ok(());
    }

    if let Some(index) = args.iter().position(|value| value == "--capture") {
        let path = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("--capture needs a path"))?;
        let size = args
            .iter()
            .position(|value| value == "--size")
            .and_then(|i| args.get(i + 1))
            .map(|value| {
                let (w, h) = value
                    .split_once('x')
                    .ok_or_else(|| anyhow!("--size expects WIDTHxHEIGHT"))?;
                Ok::<(u32, u32), anyhow::Error>((w.parse()?, h.parse()?))
            })
            .transpose()?
            .unwrap_or((CANVAS_WIDTH as u32, CANVAS_HEIGHT as u32));
        if args.iter().any(|value| value == "--title") {
            player.screen = Screen::Title;
            player.compose(size.0 as f32, size.1 as f32);
            let pixels = player
                .renderer
                .render_offscreen(size.0, size.1, [0.0, 0.0, 0.0, 1.0])?;
            image::save_buffer(
                path,
                &pixels,
                size.0,
                size.1,
                image::ExtendedColorType::Rgba8,
            )?;
            println!("captured title -> {path}");
            return Ok(());
        }
        return player.capture(Path::new(path), &options, size);
    }

    if options.starts_game() {
        player.prepare_debug(&options, (WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32))?;
        if let Some(bgm) = player.runtime.state.bgm.clone() {
            player.audio.play_bgm(&player.assets, &bgm, true)?;
        }
    }
    let event_loop = winit::event_loop::EventLoop::new()?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
    let mut app = App::new(player);
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod audio_ui_regressions {
    use super::*;

    fn audit_player() -> Option<Player> {
        if std::env::var_os("KOIKARI_METAL_TESTS").as_deref() != Some(std::ffi::OsStr::new("1")) {
            return None;
        }
        let root = std::env::var_os("KOIKARI_GAME_ROOT")?;
        let mut player = Player::new(Path::new(&root), TEXTURE_BUDGET, true).unwrap();
        player.audio_notice.clear();
        Some(player)
    }
    fn synthetic_runtime(source: &str) -> Runtime {
        Runtime::from_scripts(
            std::collections::BTreeMap::from([("main.ks".into(), source.into())]),
            "main.ks",
        )
        .unwrap()
    }

    #[test]
    fn start_choose_and_loaded_blocked_states_always_keep_the_vm_diagnostic_visible() {
        let Some(mut player) = audit_player() else {
            return;
        };
        player.runtime = synthetic_runtime("[unknown_semantic]");
        player.start_game();
        assert_eq!(player.screen, Screen::Game);
        assert!(
            player
                .persistent_diagnostic()
                .unwrap()
                .contains("unknown command")
        );
        player.runtime =
            synthetic_runtime("[choice text=go target='*bad'][s]\n*bad\n[unknown_semantic]");
        assert_eq!(player.runtime.start().unwrap(), Event::Choices);
        player.choose(0);
        assert!(
            player
                .persistent_diagnostic()
                .unwrap()
                .contains("unknown command")
        );
        let blocked = player.runtime.snapshot();
        player.runtime =
            synthetic_runtime("[choice text=go target='*bad'][s]\n*bad\n[unknown_semantic]");
        player.runtime.restore(blocked).unwrap();
        player.status = None;
        player.storage_feedback(Ok(()), "已读取快速存档", "读档失败", Instant::now());
        assert!(
            player
                .persistent_diagnostic()
                .unwrap()
                .contains("unknown command")
        );
        let notice = visible_notice(
            player.persistent_diagnostic(),
            &player.audio_notice,
            &player.success_notice,
            Instant::now(),
        )
        .unwrap();
        assert_eq!(notice.0, NoticeKind::Error);
    }

    #[test]
    fn title_save_cannot_touch_progress_and_loading_is_bounded_and_transactional() {
        let Some(mut player) = audit_player() else {
            return;
        };
        let temp_root = std::env::temp_dir().canonicalize().unwrap();
        let directory = temp_root.join(format!(
            "koikari-controller-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let expected_directory = directory.canonicalize().unwrap();
        assert_eq!(expected_directory.parent(), Some(temp_root.as_path()));
        let file = directory.join("quicksave.json");
        let mut seed = synthetic_runtime("[地]protected progress[c]");
        seed.start().unwrap();
        koikari_metal::save::write_snapshot(&file, &seed.snapshot()).unwrap();
        let protected = std::fs::read(&file).unwrap();
        player.runtime = synthetic_runtime("[地]new progress[c]");
        player.screen = Screen::Title;
        let called = std::cell::Cell::new(false);
        player.quick_save_with_directory(|| {
            called.set(true);
            Ok(directory.clone())
        });
        assert!(!called.get(), "title F5 must not even resolve a save path");
        assert!(std::fs::read(&file).unwrap() == protected);
        player.screen = Screen::Game;
        player.quick_save_with_directory(|| {
            called.set(true);
            Ok(directory.clone())
        });
        assert!(
            !called.get(),
            "unstarted states must not replace progress either"
        );
        player.start_game();
        player.quick_save_with_directory(|| Ok(directory.clone()));
        assert_eq!(
            koikari_metal::save::read_snapshot(&file)
                .unwrap()
                .state
                .text,
            "new progress"
        );
        let latest = std::fs::read(&file).unwrap();
        player.runtime.state.text = "unsaved title state".into();
        player.screen = Screen::Title;
        player.quick_save_with_directory(|| panic!("title screen must not invoke storage"));
        assert!(std::fs::read(&file).unwrap() == latest);
        let before = player.runtime.snapshot();
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&file)
            .unwrap()
            .set_len(koikari_metal::save::MAX_SAVE_BYTES + 1)
            .unwrap();
        player.quick_load_from_directory(Ok(directory.clone()));
        assert!(player.save_error.as_deref().unwrap().contains("16 MiB"));
        assert!(
            player.runtime.snapshot() == before,
            "failed load must preserve every live-state identity"
        );
        assert_eq!(directory.canonicalize().unwrap(), expected_directory);
        assert!(
            expected_directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("koikari-controller-audit-")
        );
        assert_eq!(
            file.canonicalize().unwrap(),
            expected_directory.join("quicksave.json")
        );
        std::fs::remove_file(&file).unwrap();
        std::fs::remove_dir(&expected_directory).unwrap();
    }

    #[test]
    fn gameplay_shortcuts_do_not_escape_the_title_and_auto_pauses_on_vm_errors() {
        let Some(player) = audit_player() else {
            return;
        };
        let mut app = App::new(player);
        for text in ["a", "h", "b"] {
            app.key(&winit::keyboard::Key::Character(text.into()), true);
        }
        assert_eq!(app.player.screen, Screen::Title);
        assert!(!app.player.auto && !app.player.hide_message);
        app.player.runtime = synthetic_runtime("[unknown_semantic]");
        app.player.start_game();
        app.player.auto = true;
        app.player.skip = true;
        let before = app.player.runtime.snapshot();
        let last_step = app.player.last_step;
        assert!(app.tick().is_none());
        assert!(app.player.runtime.snapshot() == before);
        assert_eq!(app.player.last_step, last_step);
        app.player.runtime = synthetic_runtime("[地]normal[c]");
        app.player.start_game();
        app.player.auto = false;
        app.player.skip = false;
        app.key(&winit::keyboard::Key::Character("h".into()), true);
        assert!(app.player.hide_message);
        app.key(&winit::keyboard::Key::Character("b".into()), true);
        assert_eq!(app.player.screen, Screen::History);
        app.key(&winit::keyboard::Key::Character("b".into()), true);
        assert_eq!(app.player.screen, Screen::Game);
        app.key(&winit::keyboard::Key::Character("a".into()), true);
        assert!(app.player.auto);
    }

    #[test]
    fn additional_resource_profile_keeps_legacy_save_path_untouched() {
        let home = Path::new("/example-home");
        let base = default_save_dir(home, false);
        assert_eq!(base, home.join("Library/Application Support/KoikariMetal"));
        assert_eq!(default_save_dir(home, true), base.join("with-additional"));
    }

    #[test]
    fn save_load_feedback_is_transient_in_the_real_compositor_and_failures_remain_visible() {
        if std::env::var_os("KOIKARI_METAL_TESTS").as_deref() != Some(std::ffi::OsStr::new("1")) {
            return;
        }
        let Some(root) = std::env::var_os("KOIKARI_GAME_ROOT") else {
            return;
        };
        let mut player = Player::new(Path::new(&root), TEXTURE_BUDGET, true).unwrap();
        player.audio_notice.clear();
        player.compose(1280.0, 720.0);
        let baseline = player
            .renderer
            .render_offscreen(1280, 720, [0.0, 0.0, 0.0, 1.0])
            .unwrap();
        let now = Instant::now();
        assert!(player.storage_feedback(Ok(()), "已快速存档 (F9 读取)", "存档失败", now));
        assert!(player.status.is_none() && player.save_error.is_none());
        player.compose(1280.0, 720.0);
        let visible = player
            .renderer
            .render_offscreen(1280, 720, [0.0, 0.0, 0.0, 1.0])
            .unwrap();
        assert_ne!(
            baseline, visible,
            "successful save must show feedback initially"
        );
        player.success_notice.show_success(
            "已快速存档 (F9 读取)".into(),
            Instant::now() - Duration::from_secs(3),
        );
        player.compose(1280.0, 720.0);
        let expired = player
            .renderer
            .render_offscreen(1280, 720, [0.0, 0.0, 0.0, 1.0])
            .unwrap();
        assert_eq!(
            baseline, expired,
            "expired save feedback must leave no banner in the rendered frame"
        );
        if let Some(dir) = std::env::var_os("KOIKARI_NOTICE_CAPTURE_DIR") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (name, pixels) in [
                ("save-success-visible.png", &visible),
                ("save-success-expired.png", &expired),
            ] {
                image::save_buffer(
                    dir.join(name),
                    pixels,
                    1280,
                    720,
                    image::ExtendedColorType::Rgba8,
                )
                .unwrap();
            }
        }
        assert!(!player.storage_feedback(
            Err(anyhow!("test write failure")),
            "已快速存档 (F9 读取)",
            "存档失败",
            now
        ));
        assert!(player.save_error.as_deref().unwrap().contains("存档失败"));
        assert!(player.success_notice.active_text(now).is_none());
        let persistent = visible_notice(
            player.status.as_deref().or(player.save_error.as_deref()),
            &player.audio_notice,
            &player.success_notice,
            now + Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(persistent.0, NoticeKind::Error);
        assert!(player.storage_feedback(Ok(()), "已读取快速存档", "读档失败", now));
        assert!(player.save_error.is_none());
        assert_eq!(
            player.success_notice.active_text(now),
            Some("已读取快速存档")
        );
        assert!(
            player
                .success_notice
                .active_text(now + Duration::from_millis(2500))
                .is_none()
        );
        player.status = Some("test script failure".into());
        assert!(player.storage_feedback(Ok(()), "已快速存档 (F9 读取)", "存档失败", now));
        assert_eq!(player.status.as_deref(), Some("test script failure"));
    }

    #[test]
    fn reported_missing_voices_do_not_create_a_red_status_or_block_story_progress() {
        if std::env::var_os("KOIKARI_METAL_TESTS").as_deref() != Some(std::ffi::OsStr::new("1")) {
            eprintln!("audio UI regression skipped: set KOIKARI_METAL_TESTS=1");
            return;
        }
        let Some(root) = std::env::var_os("KOIKARI_GAME_ROOT") else {
            eprintln!("audio UI regression skipped: set KOIKARI_GAME_ROOT");
            return;
        };
        let mut player = Player::new(Path::new(&root), TEXTURE_BUDGET, true).unwrap();
        player.audio.set_volumes(koikari_metal::audio::Volumes {
            bgm: 0.0,
            voice: 0.0,
            se: 0.0,
        });
        for (scenario, voice) in [
            ("scenario/main/5_4.ks", "vo5_1089"),
            ("scenario/main/5_6.ks", "vo5_1169"),
        ] {
            player.runtime.start_at(scenario, None).unwrap();
            let mut found = false;
            for _ in 0..4000 {
                assert!(player.runtime.state.diagnostic.is_none());
                if player.runtime.state.voice.as_deref() == Some(voice) {
                    found = true;
                    break;
                }
                player.runtime.take_effects();
                assert_ne!(player.runtime.advance().unwrap(), Event::Blocked);
            }
            assert!(
                found,
                "reported missing voice stop was not reached: {voice}"
            );
            player.status = None;
            player.audio_notice.clear();
            player.screen = Screen::Game;
            // Exercise the exact normal UI path, not the debug capture's silent
            // effect drain that would never have reproduced the old error.
            player.apply_effects();
            assert!(player.status.is_none());
            assert!(player.audio_notice.active_text(Instant::now()).is_none());
            assert!(player.runtime.state.waiting);
            let before = player.runtime.location().clone();
            // A stale recoverable audio notice must disappear at the next line.
            player.report_audio_notice("audio: simulated old recoverable warning".into());
            player.advance();
            assert_ne!(player.runtime.location(), &before);
            assert!(player.status.is_none());
            assert!(player.audio_notice.active_text(Instant::now()).is_none());
        }
    }
}
