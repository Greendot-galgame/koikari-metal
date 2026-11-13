//! Steam-free native macOS runtime for locally owned Koikari data.
//!
//! The shipped game is a Windows/i386 KiriKiri application. This crate keeps the
//! player's original `data.xp3`/`patch.xp3` untouched and re-implements the
//! resource access, scenario execution, rendering and audio on top of Rust and
//! Metal. No Windows binary and no Steam component is loaded.

pub mod archive;
pub mod assets;
pub mod runtime;
pub mod save;
pub mod script;

#[cfg(target_os = "macos")]
pub mod audio;
#[cfg(target_os = "macos")]
pub mod renderer;
#[cfg(target_os = "macos")]
pub mod text;

/// The game's logical canvas, confirmed from `system/config.tjs`
/// (`scWidth = 1280 << 1`, `scHeight = 720 << 1`).
pub const CANVAS_WIDTH: f32 = 2560.0;
pub const CANVAS_HEIGHT: f32 = 1440.0;

/// Frames per second used for animation timelines.
pub const FRAME_RATE: f32 = 60.0;

/// Normalised case-insensitive comparison used when resolving asset aliases.
pub fn normalize_asset_key(name: &str) -> String {
    name.replace('\\', "/")
        .trim_start_matches("./")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_keys_are_platform_neutral() {
        assert_eq!(
            normalize_asset_key("./BGImage/BG1_0.WEBP"),
            "bgimage/bg1_0.webp"
        );
        assert_eq!(
            normalize_asset_key("scenario\\main\\0_1.ks"),
            "scenario/main/0_1.ks"
        );
    }
}
