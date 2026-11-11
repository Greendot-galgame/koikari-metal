//! Versioned JSON VM snapshots. No executable code or native object serialization.
use crate::runtime::Snapshot;
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const MAX_SAVE_BYTES: u64 = 16 * 1024 * 1024;

pub fn to_json(snapshot: &Snapshot) -> Result<String> {
    ensure!(
        snapshot
            .state
            .sprites
            .iter()
            .all(|sprite| sprite.valid_transform()),
        "cannot save invalid sprite transform"
    );
    let text = serde_json::to_string_pretty(snapshot).context("serialize VN snapshot")?;
    ensure!(
        text.len() as u64 <= MAX_SAVE_BYTES,
        "snapshot exceeds save-size limit"
    );
    Ok(text)
}
pub fn from_json(text: &str) -> Result<Snapshot> {
    ensure!(
        text.len() as u64 <= MAX_SAVE_BYTES,
        "save exceeds 16 MiB limit"
    );
    serde_json::from_str(text).context("invalid VN save JSON")
}
/// Write a new sibling file, flush it, then atomically replace the selected save.
/// Existing save bytes remain untouched if serialization or writing fails.
pub fn write_snapshot(path: impl AsRef<Path>, snapshot: &Snapshot) -> Result<()> {
    let path = path.as_ref();
    let data = to_json(snapshot)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create save directory {}", parent.display()))?;
    let filename = path
        .file_name()
        .context("save path has no filename")?
        .to_string_lossy();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temporary: PathBuf = parent.join(format!(".{filename}.{}.{nonce}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .context("create atomic save staging file")?;
    let result = (|| -> Result<()> {
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path).with_context(|| format!("replace save {}", path.display()))?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
pub fn read_snapshot(path: impl AsRef<Path>) -> Result<Snapshot> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("open save {}", path.display()))?;
    ensure!(
        file.metadata()?.len() <= MAX_SAVE_BYTES,
        "save exceeds 16 MiB limit"
    );
    let mut data = String::new();
    file.take(MAX_SAVE_BYTES + 1)
        .read_to_string(&mut data)
        .context("read UTF-8 save JSON")?;
    from_json(&data)
}

/// Filename only: callers choose the writable save directory, never the asset root.
pub fn slot_filename(slot: usize) -> String {
    format!("slot-{slot:03}.json")
}
