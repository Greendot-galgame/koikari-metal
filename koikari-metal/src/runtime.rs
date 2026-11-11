//! Game-specific, bounded VN VM. Original TJS and Windows/Steam code are never run.
use crate::assets::AssetStore;
use crate::script::{self, InstructionKind, Program, Tag, Variables};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

pub const FIRST_SCENARIO: &str = "scenario/main/0_1.ks";
pub const CANVAS_WIDTH: u32 = 2560;
pub const CANVAS_HEIGHT: u32 = 1440;
pub const STEP_BUDGET: usize = 10_000;
pub const SAVE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sprite {
    pub id: u32,
    pub storage: String,
    /// Draw these transparent full-size images over `storage`, at the same origin.
    pub overlays: Vec<String>,
    /// Top-left physical pixels on the 2560x1440 authored canvas.
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub opacity: f32,
    pub z: i32,
    /// The single foreground face portrait, composited above the message frame.
    /// This is explicit state, not a guess based on image size or draw order.
    #[serde(default)]
    pub portrait: bool,
}
impl Sprite {
    pub(crate) fn valid_transform(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.scale.is_finite()
            && self.scale > 0.0
            && self.opacity.is_finite()
            && (0.0..=1.0).contains(&self.opacity)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub text: String,
    pub storage: String,
    pub target: String,
    pub direct_jump: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneState {
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub background: Option<String>,
    pub sprites: Vec<Sprite>,
    pub speaker: String,
    pub text: String,
    pub choices: Vec<Choice>,
    pub bgm: Option<String>,
    pub voice: Option<String>,
    pub looping_sounds: BTreeMap<u32, String>,
    pub message_visible: bool,
    pub message_style: u32,
    pub font_size: u32,
    pub waiting: bool,
    pub ended: bool,
    pub diagnostic: Option<String>,
    /// Explicit compatibility notices; cosmetic limitations never masquerade as
    /// unknown branch decisions. Unknown semantic instructions block instead.
    pub warnings: Vec<String>,
    pub scenario: String,
    pub source_line: usize,
    pub messages_seen: u64,
}
impl Default for SceneState {
    fn default() -> Self {
        Self {
            canvas_width: CANVAS_WIDTH,
            canvas_height: CANVAS_HEIGHT,
            background: None,
            sprites: Vec::new(),
            speaker: String::new(),
            text: String::new(),
            choices: Vec::new(),
            bgm: None,
            voice: None,
            looping_sounds: BTreeMap::new(),
            message_visible: true,
            message_style: 0,
            font_size: 52,
            waiting: false,
            ended: false,
            diagnostic: None,
            warnings: Vec::new(),
            scenario: String::new(),
            source_line: 0,
            messages_seen: 0,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Dialogue,
    Choices,
    Ended,
    Blocked,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioEffect {
    Bgm {
        storage: String,
        looping: bool,
    },
    Voice {
        storage: String,
    },
    Se {
        storage: String,
        looping: bool,
        channel: u32,
    },
    /// Reset transient and looping audio on a new execution or successful load.
    StopAll,
    StopBgm,
    StopVoice,
    StopSe {
        channel: u32,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub storage: String,
    pub pc: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallFrame {
    pub return_to: Location,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub speaker: String,
    pub text: String,
    pub voice: Option<String>,
    pub storage: String,
    pub line: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredScene {
    pub background: String,
    pub bgm: Option<String>,
    pub message_visible: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub game_fingerprint: String,
    pub location: Location,
    pub call_stack: Vec<CallFrame>,
    pub variables: Variables,
    pub state: SceneState,
    pub history: Vec<HistoryEntry>,
    pub source_fingerprints: BTreeMap<String, String>,
    pub selection: Option<usize>,
    pub deferred: Option<DeferredScene>,
    pub started: bool,
}
#[derive(Clone, Debug)]
pub struct Chapter {
    pub storage: String,
    pub next: Option<String>,
}
#[derive(Clone, Debug)]
struct Selection {
    storage: String,
    texts: Vec<String>,
    destinations: Vec<Option<String>>,
}
#[derive(Clone, Debug, Default)]
struct GameDatabase {
    chapters: Vec<Chapter>,
    choices: Vec<Selection>,
    names: BTreeMap<String, String>,
    layout: Variables,
    widths: Value,
    /// Literal motion definitions only; None denotes an endless loop with no
    /// finite endpoint. The original TJS motion engine is never executed.
    motions: BTreeMap<String, Option<[f32; 2]>>,
    fingerprint: String,
}
impl GameDatabase {
    fn load(store: &AssetStore) -> Result<Self> {
        let source = store.text("scenario/DB/DB_EV.ks")?;
        let mut db = Self {
            fingerprint: script::fingerprint(&source),
            ..Self::default()
        };
        let events = script::literal_assignment(&source, "lf.EV_EventListInfo")?;
        let events = events
            .as_array()
            .ok_or_else(|| anyhow!("event table is not array"))?;
        for (i, row) in events.iter().enumerate() {
            let name = row[0]
                .as_str()
                .ok_or_else(|| anyhow!("invalid chapter {i}"))?;
            let next = match &row[3] {
                Value::String(v) if v == "End" => None,
                Value::String(v) => Some(scenario_name(v)),
                Value::Number(v) if v.as_i64() == Some(-1) => events
                    .get(i + 1)
                    .and_then(|r| r[0].as_str())
                    .map(scenario_name),
                _ => bail!("unknown next-chapter rule for {name}"),
            };
            db.chapters.push(Chapter {
                storage: scenario_name(name),
                next,
            });
        }
        let choices = script::literal_assignment(&source, "lf.EV_SelectListInfo")?;
        for row in choices
            .as_array()
            .ok_or_else(|| anyhow!("choice table is not array"))?
        {
            let storage = scenario_name(
                row[0]
                    .as_str()
                    .ok_or_else(|| anyhow!("choice without source"))?,
            );
            let texts = row[4]
                .as_array()
                .ok_or_else(|| anyhow!("choice without text"))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("nonliteral choice"))
                })
                .collect::<Result<Vec<_>>>()?;
            let destinations = row[1]
                .as_array()
                .ok_or_else(|| anyhow!("choice without destinations"))?
                .iter()
                .map(|v| {
                    let s = v
                        .as_str()
                        .ok_or_else(|| anyhow!("nonliteral choice destination"))?;
                    Ok(if s.is_empty() {
                        None
                    } else {
                        Some(scenario_name(s))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                destinations.len() >= texts.len(),
                "choice destination table too short"
            );
            db.choices.push(Selection {
                storage,
                texts,
                destinations,
            });
        }
        let names = store.text("scenario/avan/Init_VariableADV.ks")?;
        for line in names.lines() {
            if let Some((left, right)) = line.trim().split_once("=>") {
                let right = right.trim().trim_end_matches(',');
                if let (Ok(Value::String(a)), Ok(Value::String(b))) = (
                    script::evaluate(left.trim(), &Variables::new()),
                    script::evaluate(right, &Variables::new()),
                ) {
                    db.names.insert(a, b);
                }
            }
        }
        // Acquire only literal numerical assignments from the original sprite
        // configuration, not its loops, API calls, or variable initialization.
        let stand = store.text("scenario/avan/Init_VariableSTAND.ks")?;
        for line in stand.lines() {
            let line = line.trim();
            let Some((name, _)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name.starts_with("STI_NoCheekNum[") {
                // Ignore the original initialization loops (indices i/k) and
                // acquire only explicit character/direction/expression triples.
                let indices: Option<Vec<u32>> = name
                    .split('[')
                    .skip(1)
                    .map(|index| index.strip_suffix(']').and_then(|s| s.parse().ok()))
                    .collect();
                if indices.as_ref().is_none_or(|indices| indices.len() != 3) {
                    continue;
                }
                let value = line.split_once('=').unwrap().1.trim().trim_end_matches(';');
                let literal = script::evaluate(value, &Variables::new())?;
                ensure!(literal.is_number(), "nonliteral cheek override");
                db.layout.insert(name.to_owned(), literal);
                continue;
            }
            if ["STI_AX[", "STI_AY[", "STI_AH[", "STI_FCAX[", "STI_FCAY["]
                .iter()
                .any(|p| name.starts_with(p))
            {
                let index = name
                    .split('[')
                    .nth(1)
                    .and_then(|v| v.strip_suffix(']'))
                    .and_then(|v| v.parse::<usize>().ok());
                if index.is_some() {
                    db.layout
                        .insert(name.to_owned(), script::literal_assignment(line, name)?);
                }
            }
        }
        let widths = store
            .text("scenario/DB/Output/立ち絵情報.ks")?
            .replace("(const)", "");
        db.widths = script::evaluate(widths.trim(), &Variables::new())?;
        db.motions = motion_endpoints(&store.text("scenario/macro/Macro_Effect.ks")?)?;
        Ok(db)
    }
    fn next(&self, storage: &str) -> Result<Option<String>> {
        self.chapters
            .iter()
            .find(|r| same_script(&r.storage, storage))
            .map(|r| r.next.clone())
            .ok_or_else(|| anyhow!("no authored continuation for {storage}"))
    }
    fn num(&self, group: &str, id: u32, size: usize, dir: usize) -> f32 {
        self.layout
            .get(&format!("{group}[{id}]"))
            .and_then(|a| a.get(size))
            .and_then(|a| a.get(dir))
            .and_then(Value::as_i64)
            .unwrap_or(0) as f32
    }
    fn flat(&self, group: &str, id: u32, index: usize) -> f32 {
        self.layout
            .get(&format!("{group}[{id}]"))
            .and_then(|a| a.get(index))
            .and_then(Value::as_i64)
            .unwrap_or(0) as f32
    }
}
/// Read declarative keyframes, not function bodies or arbitrary TJS. The motion
/// plugin expresses keyframes relative to the initial origin; finite repetition
/// adds the last offset once per loop (not the sum of all intermediate frames).
fn motion_endpoints(source: &str) -> Result<BTreeMap<String, Option<[f32; 2]>>> {
    let mut motions = BTreeMap::new();
    let mut aliases = Vec::new();
    for token in script::tokenize(source)? {
        let script::TokenKind::Tag(tag) = token.kind else {
            continue;
        };
        if tag.name == "motion_define" {
            let Some(name) = tag.get("name") else {
                continue;
            };
            let loops = tag.get("loop").unwrap_or("1").parse::<u32>()?;
            ensure!(loops <= 1024, "motion loop count exceeds limit");
            let endpoint = if loops == 0 {
                None
            } else {
                let mut delta = [0.0; 2];
                for (axis, key) in ["locatex", "locatey"].iter().enumerate() {
                    if let Some(keyframes) = tag.get(key) {
                        let final_frame = keyframes
                            .rsplit('(')
                            .next()
                            .context("empty motion keyframes")?;
                        let value = final_frame
                            .split(',')
                            .next()
                            .context("empty motion coordinate")?
                            .trim()
                            .parse::<f32>()?;
                        ensure!(value.is_finite(), "non-finite motion coordinate");
                        delta[axis] = value * loops as f32;
                    }
                }
                Some(delta)
            };
            motions.insert(name.to_owned(), endpoint);
        } else if tag.name == "pmacro" {
            if let (Some(name), Some(id)) = (tag.get("name"), tag.get("id")) {
                aliases.push((name.to_owned(), id.to_owned()));
            }
        }
    }
    for (alias, id) in aliases {
        if let Some(endpoint) = motions.get(&id).copied() {
            motions.insert(alias, endpoint);
        }
    }
    Ok(motions)
}

fn same_script(a: &str, b: &str) -> bool {
    a.rsplit('/')
        .next()
        .unwrap_or(a)
        .eq_ignore_ascii_case(b.rsplit('/').next().unwrap_or(b))
}
fn scenario_name(name: &str) -> String {
    let name = name.trim();
    if name.contains('/') {
        if name.ends_with(".ks") {
            name.to_owned()
        } else {
            format!("{name}.ks")
        }
    } else {
        format!(
            "scenario/main/{}",
            if name.ends_with(".ks") {
                name.to_owned()
            } else {
                format!("{name}.ks")
            }
        )
    }
}

#[derive(Clone)]
pub struct Runtime {
    assets: Option<Arc<AssetStore>>,
    programs: BTreeMap<String, Program>,
    /// Dependencies of the current execution, separate from reusable code cache.
    source_dependencies: BTreeSet<String>,
    source_texts: BTreeMap<String, String>,
    database: GameDatabase,
    pub state: SceneState,
    pub history: Vec<HistoryEntry>,
    pub variables: Variables,
    location: Location,
    call_stack: Vec<CallFrame>,
    effects: Vec<AudioEffect>,
    selection: Option<usize>,
    deferred: Option<DeferredScene>,
    started: bool,
    warned: BTreeSet<String>,
}
impl Runtime {
    pub fn new(assets: Arc<AssetStore>) -> Result<Self> {
        let database = GameDatabase::load(&assets).context("read safe Koikari metadata")?;
        let mut runtime = Self::empty(database);
        runtime.assets = Some(assets);
        runtime.ensure_program(FIRST_SCENARIO)?;
        Ok(runtime)
    }
    /// A deterministic fixture VM that never touches archives or a filesystem.
    pub fn from_scripts(scripts: BTreeMap<String, String>, entry: &str) -> Result<Self> {
        let mut runtime = Self::empty(GameDatabase {
            fingerprint: "fixture-v1".into(),
            ..GameDatabase::default()
        });
        runtime.source_texts = scripts;
        runtime.location.storage = entry.to_owned();
        runtime.ensure_program(entry)?;
        Ok(runtime)
    }
    fn empty(database: GameDatabase) -> Self {
        Self {
            assets: None,
            programs: BTreeMap::new(),
            source_dependencies: BTreeSet::new(),
            source_texts: BTreeMap::new(),
            database,
            state: SceneState::default(),
            history: Vec::new(),
            variables: bootstrap_variables(),
            location: Location {
                storage: FIRST_SCENARIO.to_owned(),
                pc: 0,
            },
            call_stack: Vec::new(),
            effects: Vec::new(),
            selection: None,
            deferred: None,
            started: false,
            warned: BTreeSet::new(),
        }
    }
    pub fn chapters(&self) -> &[Chapter] {
        &self.database.chapters
    }
    pub fn location(&self) -> &Location {
        &self.location
    }
    pub fn is_started(&self) -> bool {
        self.started
    }
    pub fn start(&mut self) -> Result<Event> {
        self.new_game()
    }
    pub fn new_game(&mut self) -> Result<Event> {
        let entry = if self.assets.is_some() {
            FIRST_SCENARIO.to_owned()
        } else {
            self.location.storage.clone()
        };
        self.state = SceneState::default();
        self.source_dependencies.clear();
        self.history.clear();
        self.variables = bootstrap_variables();
        self.call_stack.clear();
        self.effects.clear();
        self.selection = None;
        self.deferred = None;
        self.location = Location {
            storage: entry,
            pc: 0,
        };
        self.started = true;
        self.warned.clear();
        self.effects.push(AudioEffect::StopAll);
        self.run()
    }
    /// Explicit chapter inspection/debug start, never inferred from lexical filenames.
    pub fn start_at(&mut self, storage: &str, target: Option<&str>) -> Result<Event> {
        self.state = SceneState::default();
        self.source_dependencies.clear();
        self.history.clear();
        self.variables = bootstrap_variables();
        self.call_stack.clear();
        self.selection = None;
        self.deferred = None;
        self.started = true;
        self.warned.clear();
        self.effects.clear();
        self.effects.push(AudioEffect::StopAll);
        self.go(storage, target)?;
        self.run()
    }
    fn can_retry_upgraded_scene_command(&self) -> bool {
        let Some(diagnostic) = self.state.diagnostic.as_deref() else {
            return false;
        };
        let Some(instruction) = self
            .programs
            .get(&self.location.storage)
            .and_then(|program| program.instructions.get(self.location.pc))
        else {
            return false;
        };
        let InstructionKind::Command(tag) = &instruction.kind else {
            return false;
        };
        if !matches!(
            tag.name.to_ascii_lowercase().as_str(),
            "sc_scenestartprocess" | "sc_sceneend" | "whiteout"
        ) {
            return false;
        }
        diagnostic
            == format!(
                "{}:{}: unknown command [{}]; no implicit no-op or guessed branch",
                self.location.storage, instruction.line, tag.name
            )
    }

    pub fn advance(&mut self) -> Result<Event> {
        if !self.started {
            return self.new_game();
        }
        if self.state.diagnostic.is_some() {
            if !self.can_retry_upgraded_scene_command() {
                return Ok(Event::Blocked);
            }
            // Old saves can contain the exact unsupported-command stop. Retry
            // that same instruction after this engine upgrade, never skip it or
            // discard unrelated errors/unknown commands.
            self.state.diagnostic = None;
            self.state.waiting = false;
            return self.run();
        }
        if self.state.ended {
            return Ok(Event::Ended);
        }
        if !self.state.choices.is_empty() {
            return Ok(Event::Choices);
        }
        self.state.waiting = false;
        self.state.text.clear();
        self.state.speaker.clear();
        self.state.voice = None;
        self.state.font_size = 52;
        self.effects.push(AudioEffect::StopVoice);
        if let Some(deferred) = self.deferred.take() {
            self.state.background = Some(deferred.background);
            self.state.sprites.clear();
            self.state.message_visible = deferred.message_visible;
            self.set_bgm(deferred.bgm);
        }
        self.run()
    }
    pub fn choose(&mut self, index: usize) -> Result<Event> {
        ensure!(self.state.diagnostic.is_none(), "runtime is blocked");
        let choice = self
            .state
            .choices
            .get(index)
            .cloned()
            .ok_or_else(|| anyhow!("choice index {index} is unavailable"))?;
        self.go(&choice.storage, Some(&choice.target))?;
        self.variables.insert(
            "f.EV_SelectDirectJump".into(),
            choice
                .direct_jump
                .as_deref()
                .map(|s| {
                    Value::String(
                        s.rsplit('/')
                            .next()
                            .unwrap_or(s)
                            .trim_end_matches(".ks")
                            .to_owned(),
                    )
                })
                .unwrap_or(json!(-1)),
        );
        self.variables
            .insert("f.EV_NowSelectNum".into(), json!(self.selection));
        self.variables
            .insert("tf.EV_SelectNowJump".into(), json!(false));
        self.state.choices.clear();
        self.state.text.clear();
        self.state.speaker.clear();
        self.state.waiting = false;
        self.state.voice = None;
        self.effects.push(AudioEffect::StopVoice);
        self.run()
    }
    pub fn take_effects(&mut self) -> Vec<AudioEffect> {
        std::mem::take(&mut self.effects)
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            version: SAVE_VERSION,
            game_fingerprint: self.database.fingerprint.clone(),
            location: self.location.clone(),
            call_stack: self.call_stack.clone(),
            variables: self.variables.clone(),
            state: self.state.clone(),
            history: self.history.clone(),
            source_fingerprints: self
                .source_dependencies
                .iter()
                .map(|name| (name.clone(), self.programs[name].fingerprint.clone()))
                .collect(),
            selection: self.selection,
            deferred: self.deferred.clone(),
            started: self.started,
        }
    }
    pub fn restore(&mut self, snapshot: Snapshot) -> Result<()> {
        // Validation may compile previously unseen code. Stage all changes so
        // rejecting a save cannot alter live caches, dependencies or effects.
        let mut candidate = self.clone();
        candidate.restore_validated(snapshot)?;
        *self = candidate;
        Ok(())
    }
    fn restore_validated(&mut self, mut snapshot: Snapshot) -> Result<()> {
        ensure!(
            snapshot.version == SAVE_VERSION,
            "unsupported save version {}",
            snapshot.version
        );
        ensure!(
            snapshot.game_fingerprint == self.database.fingerprint,
            "save belongs to different game metadata"
        );
        ensure!(
            snapshot.call_stack.len() <= 64
                && snapshot.history.len() <= 2000
                && snapshot.variables.len() <= 16384,
            "save exceeds VM limits"
        );
        ensure!(
            snapshot.state.canvas_width == CANVAS_WIDTH
                && snapshot.state.canvas_height == CANVAS_HEIGHT,
            "invalid save canvas"
        );
        let mut dependencies = BTreeSet::new();
        for (name, expected) in &snapshot.source_fingerprints {
            let actual = self.ensure_program(name)?;
            dependencies.insert(actual.clone());
            ensure!(
                &self.programs[&actual].fingerprint == expected,
                "scenario changed since save: {name}"
            );
        }
        for location in std::iter::once(&snapshot.location)
            .chain(snapshot.call_stack.iter().map(|f| &f.return_to))
        {
            let name = self.ensure_program(&location.storage)?;
            ensure!(
                snapshot.source_fingerprints.contains_key(&name),
                "save omits scenario identity: {name}"
            );
            ensure!(
                location.pc <= self.programs[&name].instructions.len(),
                "save program counter is out of range"
            );
        }
        ensure!(
            snapshot.state.sprites.iter().all(Sprite::valid_transform),
            "invalid save sprite transform"
        );
        // Version-one saves did not distinguish face portraits and could contain
        // several characters in the same face slot. Recover that explicit state
        // before exposing the old save to the compositor, keeping the newest one.
        for sprite in &mut snapshot.state.sprites {
            if sprite.z == 100_000 && sprite.storage.contains("_s3_") {
                sprite.portrait = true;
            }
        }
        let last_portrait = snapshot.state.sprites.iter().rposition(|s| s.portrait);
        let mut index = 0;
        snapshot.state.sprites.retain(|s| {
            let keep = !s.portrait || Some(index) == last_portrait;
            index += 1;
            keep
        });
        self.effects.clear();
        self.effects.push(AudioEffect::StopAll);
        self.source_dependencies = dependencies;
        self.state = snapshot.state;
        self.history = snapshot.history;
        self.variables = snapshot.variables;
        self.location = snapshot.location;
        self.call_stack = snapshot.call_stack;
        self.selection = snapshot.selection;
        self.deferred = snapshot.deferred;
        self.started = snapshot.started;
        self.warned = self.state.warnings.iter().cloned().collect();
        if let Some(storage) = &self.state.bgm {
            self.effects.push(AudioEffect::Bgm {
                storage: storage.clone(),
                looping: true,
            });
        }
        if let Some(storage) = &self.state.voice {
            self.effects.push(AudioEffect::Voice {
                storage: storage.clone(),
            });
        }
        for (channel, storage) in &self.state.looping_sounds {
            self.effects.push(AudioEffect::Se {
                storage: storage.clone(),
                looping: true,
                channel: *channel,
            });
        }
        Ok(())
    }
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        crate::save::write_snapshot(path, &self.snapshot())
    }
    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.restore(crate::save::read_snapshot(path)?)
    }
    fn ensure_program(&mut self, requested: &str) -> Result<String> {
        if self.programs.contains_key(requested) {
            self.source_dependencies.insert(requested.to_owned());
            return Ok(requested.to_owned());
        }
        let (name, text) = if let Some(store) = &self.assets {
            let name = store.resolve(requested)?;
            let text = store.text(&name)?;
            (name, text)
        } else {
            let name = if self.source_texts.contains_key(requested) {
                requested.to_owned()
            } else {
                let matches: Vec<_> = self
                    .source_texts
                    .keys()
                    .filter(|n| same_script(n, requested))
                    .cloned()
                    .collect();
                ensure!(
                    matches.len() == 1,
                    "scenario {requested} absent or ambiguous"
                );
                matches[0].clone()
            };
            (name.clone(), self.source_texts[&name].clone())
        };
        if !self.programs.contains_key(&name) {
            self.programs.insert(
                name.clone(),
                Program::compile(&name, &text).with_context(|| format!("compile {name}"))?,
            );
        }
        self.source_dependencies.insert(name.clone());
        Ok(name)
    }
    fn go(&mut self, storage: &str, target: Option<&str>) -> Result<()> {
        let name = self.ensure_program(storage)?;
        let pc = if let Some(target) = target.filter(|s| !s.is_empty()) {
            self.programs[&name].label_pc(target)?
        } else {
            0
        };
        self.location = Location { storage: name, pc };
        Ok(())
    }
    fn run(&mut self) -> Result<Event> {
        for _ in 0..STEP_BUDGET {
            let source = self.location.storage.clone();
            let name = match self.ensure_program(&source) {
                Ok(n) => n,
                Err(e) => return Ok(self.block(format!("{e:#}"))),
            };
            self.location.storage = name.clone();
            self.state.scenario = name.clone();
            let Some(op) = self.programs[&name]
                .instructions
                .get(self.location.pc)
                .cloned()
            else {
                if let Some(frame) = self.call_stack.pop() {
                    self.location = frame.return_to;
                    continue;
                }
                self.state.ended = true;
                self.state.waiting = false;
                return Ok(Event::Ended);
            };
            let before = self.location.clone();
            self.location.pc += 1;
            self.state.source_line = op.line;
            let action: Result<Option<Event>> = match op.kind {
                InstructionKind::Text(text) => {
                    self.state.text.push_str(&text);
                    Ok(None)
                }
                InstructionKind::JumpPc(pc) => {
                    self.location.pc = pc;
                    Ok(None)
                }
                InstructionKind::Branch {
                    expression,
                    false_pc,
                } => script::evaluate(&expression, &self.variables).and_then(|v| {
                    if !script::truthy(&v)? {
                        self.location.pc = false_pc;
                    }
                    Ok(None)
                }),
                InstructionKind::Unsupported(reason) => {
                    Err(anyhow!("unsupported semantic: {reason}"))
                }
                InstructionKind::Command(tag) => self.command(&tag),
            };
            match action {
                Ok(Some(event)) => return Ok(event),
                Ok(None) => {}
                Err(e) => {
                    self.location = before;
                    return Ok(self.block(format!("{}:{}: {e:#}", name, op.line)));
                }
            }
        }
        Ok(self.block(format!(
            "instruction budget ({STEP_BUDGET}) exceeded at {}:{}",
            self.location.storage, self.state.source_line
        )))
    }
    fn block(&mut self, reason: String) -> Event {
        self.state.diagnostic = Some(reason);
        self.state.waiting = true;
        Event::Blocked
    }
    fn warn(&mut self, reason: &str) {
        if self.warned.insert(reason.to_owned()) && self.state.warnings.len() < 128 {
            self.state.warnings.push(reason.to_owned());
        }
    }
    fn arg(&self, tag: &Tag, key: &str) -> Result<Option<String>> {
        let Some(s) = tag.get(key) else {
            return Ok(None);
        };
        if let Some(exp) = s.strip_prefix('&') {
            Ok(Some(script::display_value(&script::evaluate(
                exp,
                &self.variables,
            )?)))
        } else {
            Ok(Some(s.to_owned()))
        }
    }
    fn number(&self, tag: &Tag, key: &str, default: i64) -> Result<i64> {
        match self.arg(tag, key)? {
            Some(v) => v
                .parse()
                .with_context(|| format!("invalid numeric {key}={v}")),
            None => Ok(default),
        }
    }
    fn boolean(&self, tag: &Tag, key: &str, default: bool) -> Result<bool> {
        match self.arg(tag, key)?.as_deref() {
            Some("true" | "1") => Ok(true),
            Some("false" | "0") => Ok(false),
            None => Ok(tag.flag(key) || default),
            Some(v) => bail!("invalid boolean {key}={v}"),
        }
    }
    fn set_bgm(&mut self, storage: Option<String>) {
        if self.state.bgm == storage {
            return;
        }
        self.state.bgm = storage.clone();
        self.effects.push(match storage {
            Some(storage) => AudioEffect::Bgm {
                storage,
                looping: true,
            },
            None => AudioEffect::StopBgm,
        });
    }
    fn apply_bgm_arg(&mut self, tag: &Tag, key: &str) -> Result<()> {
        if let Some(v) = self.arg(tag, key)? {
            self.set_bgm(if v == "stop" || v == "false" || v.is_empty() {
                None
            } else {
                Some(v)
            });
        }
        Ok(())
    }
    fn dialogue(&mut self) -> Event {
        self.state.waiting = true;
        self.state.messages_seen = self.state.messages_seen.saturating_add(1);
        let line_count = self
            .variables
            .get("f.ADV_SerifLineCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.variables.insert(
            "f.ADV_SerifLineCount".into(),
            json!(line_count.saturating_add(1)),
        );
        self.history.push(HistoryEntry {
            speaker: self.state.speaker.clone(),
            text: self.state.text.clone(),
            voice: self.state.voice.clone(),
            storage: self.location.storage.clone(),
            line: self.state.source_line,
        });
        if self.history.len() > 2000 {
            self.history.remove(0);
        }
        Event::Dialogue
    }
    fn set_speaker(&mut self, name: &str, tag: &Tag) -> Result<()> {
        let name = name.trim_start_matches('【').trim_end_matches('】');
        self.state.speaker = self
            .database
            .names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_owned());
        self.state.font_size = self.number(tag, "fs", 26)?.clamp(8, 96) as u32 * 2;
        self.state.voice = self.arg(tag, "vo")?;
        self.effects.push(AudioEffect::StopVoice);
        if let Some(storage) = &self.state.voice {
            self.effects.push(AudioEffect::Voice {
                storage: storage.clone(),
            });
        }
        Ok(())
    }
    /// The original scene macro is NOT a chapter-start alias: without `bg`
    /// it leaves the inherited picture, music, sprites and message window alone.
    fn scene_start(&mut self, tag: &Tag) -> Result<()> {
        self.require_story_scene_mode()?;
        let background = self.arg(tag, "bg")?;
        let bgm = if background.is_some() {
            self.arg(tag, "bgm")?
        } else {
            None
        };
        self.variables.insert("f.HSceneCheck".into(), json!(true));
        if let Some(background) = background {
            // Ordinary mode expands to bgset, whose final message style is zero.
            self.state.sprites.clear();
            self.state.background = Some(background);
            self.state.message_visible = true;
            self.state.message_style = 0;
            self.variables.insert("f.MS_BaseImageType".into(), json!(0));
            if let Some(bgm) = bgm {
                self.set_bgm(if matches!(bgm.as_str(), "stop" | "false" | "") {
                    None
                } else {
                    Some(bgm)
                });
            }
        }
        self.warn("Original gallery/read-history and scene-specific audio settings are not integrated; normal story scene state is preserved.");
        Ok(())
    }

    fn scene_end(&mut self) -> Result<()> {
        self.require_story_scene_mode()?;
        self.variables.insert("f.HSceneCheck".into(), json!(false));
        Ok(())
    }

    fn require_story_scene_mode(&self) -> Result<()> {
        let replay = self
            .variables
            .get("tf.SCENE_Enabled")
            .map(script::truthy)
            .transpose()?
            .unwrap_or(false);
        ensure!(
            !replay,
            "original scene-replay navigation is not implemented; refusing to guess its ADV_Start.ks target"
        );
        Ok(())
    }

    fn command(&mut self, tag: &Tag) -> Result<Option<Event>> {
        if let Some(cond) = tag.get("cond") {
            if !script::truthy(&script::evaluate(cond, &self.variables)?)? {
                return Ok(None);
            }
        }
        let name = tag.name.to_ascii_lowercase();
        match name.as_str() {
            "c" | "p" | "l" => return Ok(Some(self.dialogue())),
            ">>" => { self.state.text.push_str(tag.get("tx").unwrap_or("「")); }
            "<<" => { self.state.text.push_str(tag.get("tx").unwrap_or("」")); }
            "r" => self.state.text.push('\n'),
            "er" | "cm" | "ct" => { self.state.text.clear(); self.state.speaker.clear(); }
            "地" => { self.set_speaker("",tag)?; }
            "ネーム表示" | "ネーム表示_ボイス" | "複数" | "モブ" | "speaker" | "name" => {
                let speaker = self.arg(tag,"text")?.or(self.arg(tag,"name")?).unwrap_or_default(); self.set_speaker(&speaker,tag)?;
            }
            "sc_startprocess" => {
                self.state.sprites.clear(); self.state.message_visible = true;
                self.state.message_style = self.number(tag,"ms",0)? as u32;
                self.variables.insert("f.ADV_SerifLineCount".into(), json!(0));
                self.variables.insert("f.MS_BaseImageType".into(), json!(self.state.message_style));
                self.state.background = self.arg(tag,"bg")?;
                if tag.get("bgm").is_none() { self.set_bgm(None); } else { self.apply_bgm_arg(tag,"bgm")?; }
            }
            "sc_scenestartprocess" => self.scene_start(tag)?,
            "sc_sceneend" => self.scene_end()?,
            "whiteout" => {
                self.state.background = Some(self.arg(tag,"bg")?.ok_or_else(|| anyhow!("whiteout needs its final background"))?);
                self.warn("White-flash transition timing is collapsed; its authored final background is restored without clearing other scene layers.");
            }
            "bg_image" | "bg" => { self.state.background = self.arg(tag,"num")?.or(self.arg(tag,"storage")?); }
            "bgset" | "bgset_withoutblack" | "bgset_mshow" | "bgset_interval" => {
                self.state.sprites.clear(); self.state.message_visible = true;
                self.state.background = if name == "bgset_interval" { self.arg(tag,"bg2")? } else { self.arg(tag,"bg")? };
                self.state.message_style = self.number(tag,"ms",if name == "bgset" { 0 } else { self.state.message_style as i64 })? as u32;
                self.apply_bgm_arg(tag,"bgm")?;
                if let Some(storage) = self.arg(tag,"se")? { self.effects.push(AudioEffect::Se { storage: format!("se{storage}"),looping: false,channel: 0 }); }
            }
            "bgset_black" => { self.state.background = Some("bgblack".into()); if tag.get("bgm") == Some("stop") { self.set_bgm(None); } }
            "stc" | "st_base" => self.sprite(tag)?,
            "hh" | "free_lay_reset" => self.state.sprites.clear(),
            "ms_hide" => { self.state.message_visible = false; if !tag.flag("standhide") { self.state.sprites.clear(); } }
            "ms_show" => { self.state.message_visible = true; self.state.message_style = self.number(tag,"num",self.state.message_style as i64)? as u32; }
            "ms_c" | "ms_change" => { self.state.message_style = self.number(tag,"num",0)? as u32; self.state.message_visible = true; }
            "bgm_p" | "bgm_fp" | "bgm_fx" | "playbgm" | "fadeinbgm" | "xchgbgm" => {
                let storage = self.arg(tag,"st")?.or(self.arg(tag,"storage")?).ok_or_else(|| anyhow!("BGM without storage"))?;
                self.state.bgm = Some(storage.clone()); self.effects.push(AudioEffect::Bgm { storage,looping: self.boolean(tag,"loop",true)? });
            }
            "bgm_fe" | "bgm_fe_bl" | "bgm_stop" | "stopbgm" | "fadeoutbgm" => self.set_bgm(None),
            "se" | "playse" => {
                let channel = self.number(tag,"buf",0)? as u32;
                if tag.get("stop").is_some() || tag.flag("stop") { self.state.looping_sounds.remove(&channel); self.effects.push(AudioEffect::StopSe { channel }); }
                else if let Some(storage) = self.arg(tag,"play")?.or(self.arg(tag,"storage")?) {
                    let looping = self.boolean(tag,"loop",false)?;
                    if looping { self.state.looping_sounds.insert(channel,storage.clone()); } else { self.state.looping_sounds.remove(&channel); }
                    self.effects.push(AudioEffect::Se { storage,looping,channel });
                }
            }
            "stopse" | "fadeoutse" => { let channel = self.number(tag,"buf",0)? as u32; self.state.looping_sounds.remove(&channel); self.effects.push(AudioEffect::StopSe { channel }); }
            "stopvoice" | "voiceallstop" => { self.state.voice = None; self.effects.push(AudioEffect::StopVoice); }
            "playvoice" => {
                let storage = self.arg(tag,"number")?.or(self.arg(tag,"storage")?).ok_or_else(|| anyhow!("voice without storage"))?;
                self.state.voice = Some(storage.clone()); self.effects.push(AudioEffect::Voice { storage });
            }
            "jump" | "call" => {
                let storage = self.arg(tag,"storage")?.unwrap_or_else(|| self.location.storage.clone());
                let target = self.arg(tag,"target")?;
                if name == "call" && storage.eq_ignore_ascii_case("Endroll.ks") && self.assets.is_some() {
                    self.warn("Original animated credit roll is not played; authored continuation is preserved.");
                } else {
                    if name == "call" { ensure!(self.call_stack.len() < 64,"call stack budget exceeded"); }
                    let frame = self.location.clone(); self.go(&storage,target.as_deref())?;
                    if name == "call" { self.call_stack.push(CallFrame { return_to: frame }); }
                }
            }
            "return" => if let Some(frame) = self.call_stack.pop() { self.location = frame.return_to; } else { self.state.ended = true; return Ok(Some(Event::Ended)); },
            "eval" => {
                let exp = tag.get("exp").ok_or_else(|| anyhow!("eval without exp"))?;
                if ["kag.cancelAutoMode()","kag.cancelSkip()"].contains(&exp) { self.warn("Original UI auto/skip control is replaced by the native UI."); }
                else {
                    let exp = if let Some(exp) = exp.strip_suffix(", SteamCheckAllRouteClear()") { self.warn("Steam achievement integration is deliberately disabled."); exp } else { exp };
                    script::assign(exp,&mut self.variables)?;
                }
            }
            "emb" => { let exp = tag.get("exp").ok_or_else(|| anyhow!("emb without exp"))?; self.state.text.push_str(&script::display_value(&script::evaluate(exp,&self.variables)?)); }
            "sl_startselect" => {
                let n = self.number(tag,"num",-1)?;
                ensure!(n >= 0,"invalid selection number"); let n = n as usize;
                let choice = self.database.choices.get(n).ok_or_else(|| anyhow!("unknown authored selection {n}"))?.clone();
                ensure!(same_script(&choice.storage,&self.location.storage),"selection belongs to different scenario");
                let program = &self.programs[&self.location.storage];
                let mut items = Vec::new();
                for (i,text) in choice.texts.iter().enumerate() {
                    let target = format!("*SLP_{n}_{i}"); program.label_pc(&target)?;
                    items.push(Choice { text: text.clone(), storage: self.location.storage.clone(),target,direct_jump: choice.destinations[i].clone() });
                }
                if tag.get("type") == Some("SP") { self.warn("Special picture selection is presented as accessible text buttons without a timeout."); }
                self.state.choices = items; self.selection = Some(n); self.state.waiting = true;
                self.state.text.clear(); self.state.speaker.clear(); self.state.voice = None;
                self.effects.push(AudioEffect::StopVoice); return Ok(Some(Event::Choices));
            }
            "sl_endselect" => { self.state.choices.clear(); self.state.text.clear(); self.state.speaker.clear(); }
            "sl_join" => {
                let n = self.selection.ok_or_else(|| anyhow!("selection join without selection"))?;
                let storage = self.database.choices.get(n).ok_or_else(|| anyhow!("unknown selection join"))?.storage.clone();
                self.go(&storage,Some(&format!("*ENDSLP_{n}")))?;
            }
            "choice" => {
                let text = self.arg(tag,"text")?.ok_or_else(|| anyhow!("choice without text"))?;
                let target = self.arg(tag,"target")?.ok_or_else(|| anyhow!("choice without target"))?;
                self.state.choices.push(Choice { text,target,storage: self.arg(tag,"storage")?.unwrap_or_else(|| self.location.storage.clone()),direct_jump: None });
            }
            "s" => { ensure!(!self.state.choices.is_empty(),"unexplained script suspension; refusing to choose a path"); self.state.waiting = true; return Ok(Some(Event::Choices)); }
            "sc_endprocess" => {
                self.scene_end()?;
                let direct = self.arg(tag,"storage")?.or_else(|| self.variables.get("tf.EV_DirectJump").and_then(Value::as_str).map(scenario_name));
                let next = if let Some(direct) = direct { Some(scenario_name(&direct)) } else { self.database.next(&self.location.storage)? };
                self.variables.insert("tf.EV_DirectJump".into(),json!(-1)); self.state.sprites.clear();
                if let Some(next) = next { self.go(&next,None)?; }
                else { self.state.ended = true; self.state.waiting = false; self.set_bgm(None); return Ok(Some(Event::Ended)); }
            }
            "eyecatch" => {
                let ending = self.boolean(tag,"end",false)?;
                let next_bg = if ending { "bgblack".to_owned() } else { self.arg(tag,"bg")?.ok_or_else(|| anyhow!("eyecatch without continuation background"))? };
                let next_bgm = if ending { None } else { self.arg(tag,"bgm")?.filter(|v| v != "stop" && v != "false") };
                self.set_bgm(None); self.state.sprites.clear(); self.state.message_visible = true;
                self.state.message_style = self.number(tag,"ms",0)? as u32;
                self.state.background = Some(self.arg(tag,"sp")?.unwrap_or_else(|| "eyecatch_Base".into()));
                if !tag.flag("nostand") && tag.get("ch").is_some() { self.sprite(tag)?; }
                self.state.speaker.clear(); self.state.text = self.arg(tag,"text")?.unwrap_or_default();
                if let Some(text) = self.arg(tag,"text2")?.filter(|v| !v.is_empty()) { self.state.text.push('\n'); self.state.text.push_str(&text); }
                self.state.voice = self.arg(tag,"voice")?;
                if let Some(storage) = &self.state.voice { self.effects.push(AudioEffect::Voice { storage: storage.clone() }); }
                self.deferred = Some(DeferredScene { background: next_bg,bgm: next_bgm,message_visible: !ending });
                self.warn("Eyecatch timing is click-driven rather than the original timed animation."); return Ok(Some(self.dialogue()));
            }
            "image" => {
                let layer = self.arg(tag,"layer")?.unwrap_or_else(|| "base".into());
                if layer == "base" { self.state.background = self.arg(tag,"storage")?; }
                else {
                    let id = layer.parse::<u32>().context("nonnumeric image layer")?;
                    let storage = self.arg(tag,"storage")?.ok_or_else(|| anyhow!("image without storage"))?;
                    self.state.sprites.retain(|s| s.id != id);
                    self.state.sprites.push(Sprite { id,storage,overlays: Vec::new(),x:self.number(tag,"left",0)? as f32,y:self.number(tag,"top",0)? as f32,scale:1.0,opacity:self.number(tag,"opacity",255)? as f32/255.0,z:self.number(tag,"index",id as i64)? as i32,portrait:false });
                }
            }
            "layopt" => {
                if let Some(layer) = self.arg(tag,"layer")?.and_then(|s| s.parse::<u32>().ok()) {
                    if !self.boolean(tag,"visible",true)? { self.state.sprites.retain(|s| s.id != layer); }
                }
            }
            "achievement" => self.warn("Steam achievement integration is deliberately disabled."),
            "t_move" => self.move_sprite(tag)?,
            "quake" => self.warn("Screen-shake animation is not reproduced."),
            "ts" | "te" | "trans" | "wt" | "backlay" | "stoptrans" => self.warn("Transitions are applied immediately; original animated timings are not reproduced."),
            "wait" | "ws" | "wv" | "wb" | "wf" | "voiceallwait" => self.warn("Noninteractive waits are collapsed into the next dialogue boundary."),
            "videolayer" | "openvideo" | "video" | "preparevideo" | "wp" | "playvideo" | "stopvideo" | "clearvideolayer" | "freeimage" => self.warn("Original opening movie is not decoded; story flow continues after its presentation commands."),
            "m_bljump_linejump" | "ms_frameopacityset" | "label" | "font" | "resetfont" | "indent" | "endindent" => {},
            _ if name.starts_with('h') && name[1..].parse::<u32>().is_ok() => { let id = name[1..].parse::<u32>()?; self.state.sprites.retain(|s| s.id != id); }
            _ if ["幸","絵未","八純","ちなつ","こなつ","椿","咲希","桃子","夢乃","吾郎","愛季","月"].contains(&tag.name.as_str()) => { self.set_speaker(&tag.name,tag)?; }
            _ => bail!("unknown command [{}]; no implicit no-op or guessed branch",tag.name),
        }
        Ok(None)
    }
    fn move_sprite(&mut self, tag: &Tag) -> Result<()> {
        let layer = self.number(tag, "lay", -1)?;
        let motion = self.arg(tag, "id")?.or_else(|| {
            tag.flags
                .iter()
                .find(|flag| self.database.motions.contains_key(*flag))
                .cloned()
        });
        let endpoint = motion
            .as_deref()
            .and_then(|id| self.database.motions.get(id))
            .copied();
        match endpoint {
            Some(Some([dx, dy])) => {
                if let Some(sprite) = self.state.sprites.iter_mut().find(|s| s.id as i64 == layer) {
                    // Storage and every overlay share this origin, so expression,
                    // cheeks and accessories cannot become detached by movement.
                    let (x, y) = (sprite.x + dx, sprite.y + dy);
                    ensure!(x.is_finite() && y.is_finite(), "sprite motion must produce finite coordinates");
                    sprite.x = x;
                    sprite.y = y;
                }
                self.warn("Sprite motion uses its authored final position; intermediate animation timing is not reproduced.");
            }
            Some(None) => self.warn("Endless sprite motion has no finite endpoint; its initial position is preserved without animation."),
            None => self.warn("Unrecognized sprite motion is not animated; existing image layers are preserved."),
        }
        Ok(())
    }

    fn sprite(&mut self, tag: &Tag) -> Result<()> {
        let id = self.number(tag, "ch", -1)?;
        ensure!((1..=49).contains(&id), "invalid sprite character {id}");
        let id = id as u32;
        if self.boolean(tag, "hide", false)? {
            self.state.sprites.retain(|s| s.id != id);
            return Ok(());
        }
        let size = tag
            .flags
            .iter()
            .find_map(|s| s.strip_prefix('s').and_then(|s| s.parse::<usize>().ok()))
            .unwrap_or(self.number(tag, "size", 2)? as usize);
        let portrait = tag.flag("fc") || tag.get("fcenabled").is_some();
        let size = if portrait { 3 } else { size };
        ensure!(size < 5, "invalid sprite size");
        let dress = tag
            .flags
            .iter()
            .find_map(|s| s.strip_prefix("dr").and_then(|s| s.parse::<usize>().ok()))
            .unwrap_or(self.number(tag, "dr", 1)? as usize);
        let face = self
            .arg(tag, "f")?
            .ok_or_else(|| anyhow!("sprite without expression"))?;
        ensure!(
            face.len() >= 3 && face.bytes().all(|c| c.is_ascii_digit()),
            "invalid encoded face {face}"
        );
        let mut dir = face[1..2].parse::<usize>()?;
        if dir > if id <= 5 { 2 } else { 1 } {
            dir = 1;
        }
        let expression = &face[2..];
        let expression_number = expression.parse::<u32>()?;
        let cheek = self
            .database
            .layout
            .get(&format!("STI_NoCheekNum[{id}][{dir}][{expression_number}]"))
            .map(script::display_value)
            .unwrap_or_else(|| face[0..1].to_owned());
        let storage = format!("stb{id}_s{size}_{dir}_{dress}");
        let mut overlays = vec![format!("st{id}_s{size}_{dir}_{expression}")];
        let cheek_image = format!("stc{id}_s{size}_{dir}_{cheek}");
        if cheek != "0" && self.asset_exists(&cheek_image) {
            overlays.push(cheek_image);
        }
        for key in ["p", "p2"] {
            if let Some(part) = self.arg(tag, key)?.filter(|s| s != "0" && !s.is_empty()) {
                let image = format!("stp{id}_s{size}_{dir}_{part}");
                if self.asset_exists(&image) && !overlays.contains(&image) {
                    overlays.push(image);
                }
            }
        }
        let old = self.state.sprites.iter().find(|s| s.id == id).cloned();
        let position = tag
            .flags
            .iter()
            .find_map(|v| position_index(v))
            .unwrap_or(self.number(tag, "px", 0)?);
        let width = self
            .database
            .widths
            .get(id as usize)
            .and_then(|a| a.get(size))
            .and_then(|a| a.get(dir))
            .and_then(Value::as_i64)
            .unwrap_or(470) as f32;
        // Original (width \ 2) << 1 is the half-width of the doubled image.
        let half_width = ((width as i64 / 2) * 2) as f32;
        let spread = match size {
            0 => 20.0,
            1 => 30.0,
            _ => 50.0 - 50.0 * (size as f32 - 2.0),
        };
        let centers = [
            1280.0,
            640.0 + spread,
            1920.0 - spread,
            320.0 + spread,
            2240.0 - spread,
            256.0,
            2048.0,
            512.0 - spread,
            1024.0,
            1536.0,
            2048.0 + spread,
            2560.0 / 6.0,
            2560.0 / 3.0,
            1280.0,
            2560.0 * 2.0 / 3.0,
            2560.0 * 5.0 / 6.0,
            2560.0 / 7.0 - spread * 2.0,
            2560.0 * 2.0 / 7.0 - spread,
            2560.0 * 3.0 / 7.0,
            2560.0 * 4.0 / 7.0,
            2560.0 * 5.0 / 7.0 + spread,
            2560.0 * 6.0 / 7.0 + spread * 2.0,
        ];
        let (mut x, mut y) = if portrait {
            (
                -180.0 + self.database.flat("STI_FCAX", id, dir),
                960.0 + self.database.flat("STI_FCAY", id, dir),
            )
        } else if position == -1 {
            old.as_ref()
                .map(|s| (s.x, s.y))
                .ok_or_else(|| anyhow!("p0 sprite has no prior position"))?
        } else {
            ensure!(
                (0..centers.len() as i64).contains(&position),
                "invalid sprite position"
            );
            let heights = [-240.0, -20.0, 0.0, 60.0, 100.0];
            (
                centers[position as usize] - half_width
                    + self.database.num("STI_AX", id, size, dir),
                self.database.flat("STI_AH", id, size)
                    + self.database.num("STI_AY", id, size, dir)
                    + heights[size],
            )
        };
        for (key, value) in [("dx", &mut x), ("dy", &mut y)] {
            if let Some(delta) = self.arg(tag, key)? {
                if delta == "s" {
                    *value = old
                        .as_ref()
                        .map(|s| if key == "dx" { s.x } else { s.y })
                        .ok_or_else(|| {
                            anyhow!("sprite coordinate inheritance without prior sprite")
                        })?;
                } else {
                    *value += delta
                        .parse::<f32>()
                        .with_context(|| format!("invalid {key}"))?;
                }
            }
        }
        ensure!(
            x.is_finite() && y.is_finite(),
            "sprite offsets must produce finite coordinates"
        );
        let opacity = self.number(tag, "opacity", 255)?.clamp(0, 255) as f32 / 255.0;
        let z = if portrait {
            100_000
        } else {
            self.number(tag, "index", id as i64 * 1000)? as i32
        };
        // The authored face UI has one active character (FaceLayerNum), not
        // one slot per character. Do not leave the previous face behind when
        // changing speakers, or when returning to an ordinary standing sprite.
        self.state.sprites.retain(|s| s.id != id && !s.portrait);
        self.state.sprites.push(Sprite {
            id,
            storage,
            overlays,
            x,
            y,
            scale: 1.0,
            opacity,
            z,
            portrait,
        });
        self.state.sprites.sort_by_key(|s| s.z);
        if tag.get("de").is_some() {
            self.warn("Delayed sprite expression changes are applied immediately, not at their original audio offset.");
        }
        if tag
            .flags
            .iter()
            .any(|s| matches!(s.as_str(), "t1" | "t2" | "t3" | "t4" | "t5"))
            || tag.get("type").is_some()
        {
            self.warn("Sprite color grading is not reproduced.");
        }
        Ok(())
    }
    fn asset_exists(&self, name: &str) -> bool {
        self.assets
            .as_ref()
            .map(|a| a.resolve(name).is_ok())
            .unwrap_or(true)
    }
}
fn position_index(s: &str) -> Option<i64> {
    [
        "pc", "pl", "pr", "pll", "prr", "plll", "prrr", "p4ll", "p4l", "p4r", "p4rr", "p5ll",
        "p5l", "p5c", "p5r", "p5rr", "p6lll", "p6ll", "p6l", "p6r", "p6rr", "p6rrr",
    ]
    .iter()
    .position(|v| *v == s)
    .map(|i| i as i64)
    .or(if s == "p0" { Some(-1) } else { None })
}
fn bootstrap_variables() -> Variables {
    BTreeMap::from([
        ("tf.EV_SelectNowJump".into(), json!(false)),
        ("tf.EV_DirectJump".into(), json!(-1)),
        ("f.EV_SelectDirectJump".into(), json!(-1)),
        ("f.EV_NowSelectNum".into(), json!(-1)),
        ("sf.MovieSkipEnabled".into(), json!(false)),
        ("sf.CF_AskALL".into(), json!(0)),
        ("f.MS_OpacityForce".into(), json!(false)),
        ("f.MS_BaseImageType".into(), json!(0)),
        ("f.ADV_SerifLineCount".into(), json!(0)),
        ("f.HSceneCheck".into(), json!(false)),
        ("tf.SCENE_Enabled".into(), json!(false)),
    ])
}

#[cfg(test)]
mod motion_tests {
    use super::*;

    #[test]
    fn motion_endpoints_use_final_offsets_times_loops_and_do_not_run_tjs() {
        let source = r#"
[motion_define name="step" locatex="(-280,200,2)" loop=1]
[pmacro name=st_l id=step move_x=-999]
[motion_define name="repeat" locatex="(-20,100,0),(-20,160,0)" loop=2]
[motion_define name="jump" locatey="(-20,130,0),(0,130,0)" loop=2]
[motion_define name="forever" locatex="(5,10,1),(0,10,1)" loop=0]
[iscript]
System.launch('never execute');
[endscript]
"#;
        let endpoints = motion_endpoints(source).unwrap();
        assert_eq!(endpoints["st_l"], Some([-280.0, 0.0]));
        assert_eq!(endpoints["repeat"], Some([-40.0, 0.0]));
        assert_eq!(endpoints["jump"], Some([0.0, 0.0]));
        assert_eq!(endpoints["forever"], None);
    }

    #[test]
    fn expression_overrides_disable_cheeks_and_duplicate_parts_are_not_blended_twice() {
        let scripts = BTreeMap::from([(
            "main.ks".into(),
            "[stc ch=3 f=1130 p=2 p2=2][地]a[c]".into(),
        )]);
        let mut runtime = Runtime::from_scripts(scripts, "main.ks").unwrap();
        runtime
            .database
            .layout
            .insert("STI_NoCheekNum[3][1][30]".into(), json!(0));
        runtime.start().unwrap();
        assert_eq!(
            runtime.state.sprites[0].overlays,
            ["st3_s2_1_30", "stp3_s2_1_2"]
        );
    }

    #[test]
    fn movement_keeps_base_expression_and_accessory_at_one_origin() {
        let scripts = BTreeMap::from([(
            "main.ks".into(),
            "[stc ch=1 f=111 p=1 pl][地]a[c]\n[t_move lay=1 st_l][地]b[c]".into(),
        )]);
        let mut runtime = Runtime::from_scripts(scripts, "main.ks").unwrap();
        runtime
            .database
            .motions
            .insert("st_l".into(), Some([-280.0, 0.0]));
        runtime.start().unwrap();
        let initial = runtime.state.sprites[0].clone();
        runtime.advance().unwrap();
        let moved = &runtime.state.sprites[0];
        assert_eq!(moved.x, initial.x - 280.0);
        assert_eq!(moved.y, initial.y);
        assert_eq!(moved.storage, initial.storage);
        assert_eq!(moved.overlays, initial.overlays);
    }
}
