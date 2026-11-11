//! Deterministic, read-only `data.xp3` -> optional `adult.xp3` -> `patch.xp3`
//! asset namespace. Later layers have higher priority.
//!
//! Search each layer from patch to base. Within a layer, prefer an exact
//! case-insensitive normalized path, then a full-path extensionless alias,
//! basename, or extensionless basename. This ordering is intentional: a patch
//! basename must override a base file even when the patch uses another prefix.
//! Ambiguous aliases fail with sorted candidates; only names proven to share
//! identical physical segments in the same archive are coalesced.

use crate::archive::{ArchiveEntry, Fingerprint, Xp3Archive, name_key};
use anyhow::{Context, Result, bail, ensure};
use encoding_rs::SHIFT_JIS;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize)]
pub struct AssetStoreStats {
    pub archive_count: usize,
    /// Unique normalized full paths across the exact-path overlay.
    pub entry_count: usize,
    /// Source entries include explicit alias entries present in archives.
    pub source_entry_count: usize,
    pub archive_bytes: u64,
    pub original_bytes: u64,
    pub stored_bytes: u64,
}

#[derive(Debug)]
pub struct AssetStore {
    root: PathBuf,
    /// Low to high precedence: data, optional additional archive, optional patch.
    archives: Vec<Xp3Archive>,
    aliases: Vec<BTreeMap<String, Vec<String>>>,
    names: Vec<String>,
    stats: AssetStoreStats,
    fingerprint: String,
}

impl AssetStore {
    /// `root` contains data.xp3, optional adult.xp3, and optional patch.xp3.
    /// Missing optional archives are fine; existing invalid archives are errors.
    /// No loose files are read or written, and no ancestor directories scanned.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_archive_set(root.as_ref(), true)
    }

    /// Keep the original data/patch namespace and fingerprints, for existing
    /// saves or installations that intentionally exclude the additional pack.
    pub fn open_base(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_archive_set(root.as_ref(), false)
    }

    fn open_archive_set(root: &Path, include_additional: bool) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("locate asset root {}", root.display()))?;
        ensure!(
            root.is_dir(),
            "asset root is not a directory: {}",
            root.display()
        );
        let mut archives = vec![Xp3Archive::open(root.join("data.xp3"))?];
        for name in ["adult.xp3", "patch.xp3"] {
            if name == "adult.xp3" && !include_additional {
                continue;
            }
            let path = root.join(name);
            match std::fs::metadata(&path) {
                Ok(metadata) => {
                    ensure!(
                        metadata.is_file(),
                        "optional archive is not a regular file: {}",
                        path.display()
                    );
                    archives.push(
                        Xp3Archive::open(&path).with_context(|| format!("open {name} overlay"))?,
                    );
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error).with_context(|| format!("inspect {name} archive")),
            }
        }
        let mut aliases = Vec::with_capacity(archives.len());
        let mut overlay = BTreeMap::new();
        let mut stats = AssetStoreStats {
            archive_count: archives.len(),
            ..AssetStoreStats::default()
        };
        let mut fingerprint = Fingerprint::new();
        fingerprint.bytes(b"koikari-asset-overlay-v1");
        for archive in &archives {
            let mut layer_aliases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for (key, entry) in archive.entries() {
                overlay.insert(key.clone(), entry.name.clone());
                for alias in alias_keys(key) {
                    layer_aliases.entry(alias).or_default().insert(key.clone());
                }
            }
            aliases.push(
                layer_aliases
                    .into_iter()
                    .map(|(alias, entries)| (alias, entries.into_iter().collect()))
                    .collect(),
            );
            let source_stats = archive.stats();
            stats.source_entry_count = stats
                .source_entry_count
                .checked_add(source_stats.entry_count)
                .context("asset entry count overflow")?;
            stats.archive_bytes = stats
                .archive_bytes
                .checked_add(source_stats.archive_bytes)
                .context("asset archive size overflow")?;
            stats.original_bytes = stats
                .original_bytes
                .checked_add(source_stats.original_bytes)
                .context("asset original size overflow")?;
            stats.stored_bytes = stats
                .stored_bytes
                .checked_add(source_stats.stored_bytes)
                .context("asset stored size overflow")?;
            fingerprint.bytes(
                archive
                    .path()
                    .file_name()
                    .context("archive filename missing")?
                    .to_string_lossy()
                    .as_bytes(),
            );
            fingerprint.bytes(&[0]);
            fingerprint.bytes(archive.fingerprint().as_bytes());
            fingerprint.bytes(&[0]);
        }
        stats.entry_count = overlay.len();
        Ok(Self {
            root,
            archives,
            aliases,
            names: overlay.into_values().collect(),
            stats,
            fingerprint: format!("koikari-assets-v1-{:016x}", fingerprint.finish()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn stats(&self) -> &AssetStoreStats {
        &self.stats
    }
    /// Archives in low-to-high priority order; each exposes path/stats/entries.
    pub fn archives(&self) -> &[Xp3Archive] {
        &self.archives
    }

    pub fn has_archive(&self, name: &str) -> bool {
        self.archives.iter().any(|archive| {
            archive
                .path()
                .file_name()
                .is_some_and(|file| file.to_string_lossy().eq_ignore_ascii_case(name))
        })
    }

    /// Stable across moving the installation. Index/size compatibility identity
    /// only, not cryptographic proof of all archived payload bytes.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn names(&self) -> Vec<String> {
        self.names.clone()
    }

    pub fn resolve(&self, name: &str) -> Result<String> {
        let (archive, key) = self.resolve_key(name)?;
        Ok(self.archives[archive].entries()[&key].name.clone())
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let (archive, key) = self.resolve_key(name)?;
        self.archives[archive].read_key(&key)
    }

    /// Optional media may be referenced by the scripts but absent from a game
    /// edition. Only true absence is None: invalid paths, ambiguous aliases and
    /// corrupt/read-failed payloads remain errors, never silent fallbacks.
    pub fn resolve_optional(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .find_key(name)?
            .map(|(archive, key)| self.archives[archive].entries()[&key].name.clone()))
    }

    pub fn read_optional(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.find_key(name)?
            .map(|(archive, key)| self.archives[archive].read_key(&key))
            .transpose()
    }

    pub fn text(&self, name: &str) -> Result<String> {
        decode_text(&self.read(name)?).with_context(|| format!("decode text asset {name:?}"))
    }

    fn resolve_key(&self, name: &str) -> Result<(usize, String)> {
        self.find_key(name)?
            .with_context(|| format!("asset {name:?} not found in loaded XP3 archives"))
    }

    fn find_key(&self, name: &str) -> Result<Option<(usize, String)>> {
        let key = name_key(name)?;
        let basename = key.rsplit('/').next().context("empty asset basename")?;
        for layer in (0..self.archives.len()).rev() {
            let archive = &self.archives[layer];
            if archive.entries().contains_key(&key) {
                return Ok(Some((layer, key)));
            }
            // Try a qualified extensionless key first, so `a/x` wins over an
            // unrelated `b/x.png` before relaxing to the basename `x`.
            if let Some(candidates) = self.aliases[layer].get(&key) {
                return Ok(Some((layer, select_alias(archive, name, candidates)?)));
            }
            if basename != key {
                if let Some(candidates) = self.aliases[layer].get(basename) {
                    return Ok(Some((layer, select_alias(archive, name, candidates)?)));
                }
            }
        }
        Ok(None)
    }
}

fn select_alias(archive: &Xp3Archive, query: &str, candidates: &[String]) -> Result<String> {
    let first_key = candidates.first().context("empty internal alias set")?;
    let first: &ArchiveEntry = &archive.entries()[first_key];
    if candidates
        .iter()
        .all(|key| first.same_storage(&archive.entries()[key]))
    {
        // Already sorted: identical physical aliases always select the same
        // canonical name, independent of index insertion/hash iteration order.
        return Ok(first_key.clone());
    }
    let names: Vec<&str> = candidates
        .iter()
        .map(|key| archive.entries()[key].name.as_str())
        .collect();
    bail!(
        "ambiguous asset alias {query:?} in {}: {}",
        archive.path().display(),
        names.join(", ")
    )
}

fn alias_keys(key: &str) -> BTreeSet<String> {
    let mut aliases = BTreeSet::new();
    let basename = key.rsplit('/').next().unwrap_or(key);
    aliases.insert(basename.to_owned());
    if let Some(stem) = without_extension(key) {
        aliases.insert(stem.to_owned());
    }
    if let Some(stem) = without_extension(basename) {
        aliases.insert(stem.to_owned());
    }
    aliases
}

fn without_extension(name: &str) -> Option<&str> {
    let last_slash = name.rfind('/').map_or(0, |i| i + 1);
    let dot = name.rfind('.')?;
    // A dotfile is a literal basename, not an empty extensionless alias.
    (dot > last_slash).then_some(&name[..dot])
}

/// Strict UTF-8 (with or without BOM), UTF-16 LE/BE, then CP932/Windows-31J.
/// Invalid explicitly BOM-marked Unicode and invalid CP932 fail rather than
/// introducing replacement characters into script commands or filenames.
pub fn decode_text(bytes: &[u8]) -> Result<String> {
    if let Some(text) = bytes.strip_prefix(b"\xef\xbb\xbf") {
        return String::from_utf8(text.to_vec()).context("invalid BOM-marked UTF-8");
    }
    if let Some(text) = bytes.strip_prefix(b"\xff\xfe") {
        return decode_utf16(text, false);
    }
    if let Some(text) = bytes.strip_prefix(b"\xfe\xff") {
        return decode_utf16(text, true);
    }
    // Older scripts sometimes have no BOM. ASCII-heavy UTF-16 can be
    // recognized reliably from alternating NULs; all other text first tries
    // UTF-8, with CP932 only as the fallback, not a lossy guess.
    if bytes.len() >= 4 && bytes.len() % 2 == 0 {
        let pairs = bytes.len() / 2;
        let even_nuls = bytes.iter().step_by(2).filter(|&&b| b == 0).count();
        let odd_nuls = bytes.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
        if odd_nuls >= pairs.div_ceil(2) && even_nuls <= pairs / 8 {
            return decode_utf16(bytes, false);
        }
        if even_nuls >= pairs.div_ceil(2) && odd_nuls <= pairs / 8 {
            return decode_utf16(bytes, true);
        }
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_owned());
    }
    let (text, malformed) = SHIFT_JIS.decode_without_bom_handling(bytes);
    ensure!(!malformed, "text is neither valid UTF-8 nor valid CP932");
    Ok(text.into_owned())
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> Result<String> {
    ensure!(bytes.len() % 2 == 0, "odd byte length in UTF-16 text");
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            let pair = [pair[0], pair[1]];
            if big_endian {
                u16::from_be_bytes(pair)
            } else {
                u16::from_le_bytes(pair)
            }
        })
        .collect();
    String::from_utf16(&units).context("invalid UTF-16 surrogate sequence")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_text_encodings() {
        let expected = "日本語の台詞\r\n";
        assert_eq!(decode_text(expected.as_bytes()).unwrap(), expected);
        let mut utf8 = b"\xef\xbb\xbf".to_vec();
        utf8.extend_from_slice(expected.as_bytes());
        assert_eq!(decode_text(&utf8).unwrap(), expected);
        for big in [false, true] {
            let mut bytes = if big {
                vec![0xfe, 0xff]
            } else {
                vec![0xff, 0xfe]
            };
            for unit in expected.encode_utf16() {
                bytes.extend_from_slice(&if big {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                });
            }
            assert_eq!(decode_text(&bytes).unwrap(), expected);
        }
        let (cp932, _, malformed) = SHIFT_JIS.encode(expected);
        assert!(!malformed);
        assert_eq!(decode_text(&cp932).unwrap(), expected);
        assert_eq!(decode_text(b"a\0b\0c\0").unwrap(), "abc");
        assert_eq!(decode_text(b"\0a\0b\0c").unwrap(), "abc");
        assert_eq!(decode_text(b"").unwrap(), "");
    }
    #[test]
    fn malformed_text_is_not_replaced() {
        for bytes in [
            &b"\xef\xbb\xbf\xff"[..],
            &b"\xff\xfea"[..],
            &b"\xff\xfe\0\xd8"[..],
            &b"\x81"[..],
        ] {
            assert!(decode_text(bytes).is_err());
        }
    }
    #[test]
    fn extension_aliases_preserve_explicit_extensions() {
        assert_eq!(without_extension("dir.ext/file.webp"), Some("dir.ext/file"));
        assert_eq!(without_extension("dir.ext/file"), None);
        assert_eq!(without_extension(".hidden"), None);
        assert_eq!(
            alias_keys("a/x.webp"),
            BTreeSet::from(["a/x".to_owned(), "x".to_owned(), "x.webp".to_owned(),])
        );
    }
}
