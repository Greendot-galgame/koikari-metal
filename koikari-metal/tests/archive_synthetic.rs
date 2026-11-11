//! Synthetic XP3 fixtures are generated in private temporary directories.
//! Nothing in these tests writes to the installation's original archives.

use anyhow::Result;
use flate2::{Compression, write::ZlibEncoder};
use koikari_metal::archive::{ArchiveLimits, XP3_MAGIC, Xp3Archive, adler32};
use koikari_metal::assets::AssetStore;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "koikari-xp3-test-{}-{nonce}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        // Only the unique directory successfully created by this fixture.
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("koikari-xp3-test-")
        );
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn compressed(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend_from_slice(&(body.len() as u64).to_le_bytes());
    bytes.extend_from_slice(body);
    bytes
}

#[derive(Clone)]
struct Seg {
    flags: u32,
    offset: u64,
    original: u64,
    stored: u64,
}
#[derive(Clone)]
struct Spec {
    name: String,
    flags: u32,
    original: u64,
    stored: u64,
    checksum: u32,
    segments: Vec<Seg>,
}
impl Spec {
    fn info(&self) -> Vec<u8> {
        let units: Vec<u16> = self.name.encode_utf16().collect();
        let mut bytes = self.flags.to_le_bytes().to_vec();
        bytes.extend_from_slice(&self.original.to_le_bytes());
        bytes.extend_from_slice(&self.stored.to_le_bytes());
        bytes.extend_from_slice(&(units.len() as u16).to_le_bytes());
        for unit in units {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }
    fn segm(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for segment in &self.segments {
            bytes.extend_from_slice(&segment.flags.to_le_bytes());
            bytes.extend_from_slice(&segment.offset.to_le_bytes());
            bytes.extend_from_slice(&segment.original.to_le_bytes());
            bytes.extend_from_slice(&segment.stored.to_le_bytes());
        }
        bytes
    }
    fn body(&self) -> Vec<u8> {
        let mut bytes = chunk(b"info", &self.info());
        bytes.extend(chunk(b"segm", &self.segm()));
        bytes.extend(chunk(b"adlr", &self.checksum.to_le_bytes()));
        bytes
    }
    fn encode(&self) -> Vec<u8> {
        chunk(b"File", &self.body())
    }
}

#[derive(Default)]
struct Fixture {
    payload: Vec<u8>,
    entries: Vec<Spec>,
}
impl Fixture {
    fn add(&mut self, name: &str, parts: &[(u32, &[u8])]) -> usize {
        let mut original = Vec::new();
        let mut segments = Vec::new();
        for &(flags, bytes) in parts {
            let stored = if flags == 1 {
                compressed(bytes)
            } else {
                bytes.to_vec()
            };
            segments.push(Seg {
                flags,
                offset: 19 + self.payload.len() as u64,
                original: bytes.len() as u64,
                stored: stored.len() as u64,
            });
            self.payload.extend_from_slice(&stored);
            original.extend_from_slice(bytes);
        }
        let index = self.entries.len();
        self.entries.push(Spec {
            name: name.to_owned(),
            flags: 0,
            original: original.len() as u64,
            stored: segments.iter().map(|s| s.stored).sum(),
            checksum: adler32(&original),
            segments,
        });
        index
    }
    fn alias(&mut self, source: usize, name: &str) {
        let mut entry = self.entries[source].clone();
        entry.name = name.to_owned();
        self.entries.push(entry);
    }
    fn index(&self) -> Vec<u8> {
        self.entries.iter().flat_map(Spec::encode).collect()
    }
    fn bytes(&self, zlib: bool) -> Vec<u8> {
        with_index(&self.payload, &self.index(), zlib)
    }
}

fn index_block(index: &[u8], zlib: bool, more: bool) -> Vec<u8> {
    let stored = if zlib {
        compressed(index)
    } else {
        index.to_vec()
    };
    let mut bytes = vec![u8::from(zlib) | if more { 0x80 } else { 0 }];
    bytes.extend_from_slice(&(stored.len() as u64).to_le_bytes());
    if zlib {
        bytes.extend_from_slice(&(index.len() as u64).to_le_bytes());
    }
    bytes.extend(stored);
    bytes
}
fn with_index(payload: &[u8], index: &[u8], zlib: bool) -> Vec<u8> {
    let mut bytes = XP3_MAGIC.to_vec();
    bytes.extend_from_slice(&(19 + payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes.extend(index_block(index, zlib, false));
    bytes
}
fn one_file() -> Fixture {
    let mut fixture = Fixture::default();
    fixture.add("Dir/台詞.ks", &[(0, b"plain bytes")]);
    fixture
}
fn opens(bytes: &[u8]) -> Result<Xp3Archive> {
    let dir = TempDir::new();
    // Keep this helper for failures only; a successful archive path is removed.
    Xp3Archive::open(dir.write("test.xp3", bytes))
}
fn assert_open_error(bytes: &[u8], fragment: &str) {
    let error = opens(bytes).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains(fragment),
        "expected {fragment:?}, got {message}"
    );
}

#[test]
fn optional_assets_distinguish_true_absence_from_ambiguity_invalid_paths_and_corruption() {
    let dir = TempDir::new();
    let mut base = Fixture::default();
    base.add("voice/vo1/present.opus", &[(0, b"base voice")]);
    base.add("voice/vo1/shared.opus", &[(0, b"one")]);
    base.add("voice/vo2/shared.opus", &[(0, b"two")]);
    let broken = base.add("voice/broken.opus", &[(0, b"corrupt")]);
    base.entries[broken].checksum ^= 1;
    dir.write("data.xp3", &base.bytes(true));
    let mut patch = Fixture::default();
    patch.add("data/voice/present.opus", &[(1, b"patched voice")]);
    dir.write("patch.xp3", &patch.bytes(true));
    let store = AssetStore::open(dir.path()).unwrap();

    assert_eq!(store.resolve_optional("absent").unwrap(), None);
    assert_eq!(store.read_optional("absent").unwrap(), None);
    assert!(
        store.read("absent").is_err(),
        "required asset reads must remain strict"
    );
    assert_eq!(
        store.read_optional("voice/vo1/present.opus").unwrap(),
        Some(b"patched voice".to_vec())
    );
    assert!(store.resolve_optional("shared").is_err());
    assert!(store.read_optional("shared").is_err());
    assert!(store.read_optional("../absent").is_err());
    assert!(store.resolve_optional("broken").unwrap().is_some());
    let error = store.read_optional("broken").unwrap_err();
    assert!(format!("{error:#}").contains("checksum mismatch"));
}

#[test]
fn reads_raw_and_zlib_indexes_and_ordered_mixed_segments() {
    for zlib in [false, true] {
        let dir = TempDir::new();
        let mut fixture = Fixture::default();
        fixture.add(
            "Dir/台詞.KS",
            &[(0, b"start:"), (1, "日本語".as_bytes()), (0, b" end")],
        );
        fixture.add("empty.raw", &[(0, b"")]);
        fixture.add("empty.zlib", &[(1, b"")]);
        fixture.add("no-segments", &[]);
        let archive = Xp3Archive::open(dir.write("test.xp3", &fixture.bytes(zlib))).unwrap();
        assert_eq!(
            archive.read("./DIR\\台詞.ks").unwrap(),
            "start:日本語 end".as_bytes()
        );
        for name in ["empty.raw", "empty.zlib", "no-segments"] {
            assert!(archive.read(name).unwrap().is_empty());
        }
        assert!(
            archive.read("台詞.KS").is_err(),
            "archive itself must not guess aliases"
        );
        assert_eq!(archive.stats().entry_count, 4);
        assert_eq!(archive.stats().segment_count, 5);
        assert_eq!(archive.stats().compressed_segment_count, 2);
        let entry = archive.entry("dir/台詞.ks").unwrap().unwrap();
        assert_eq!(entry.segments[0].offset, 19);
        assert_eq!(entry.segments[0].original_size, 6);
        assert_eq!(entry.name, "Dir/台詞.KS");
    }
}

#[test]
fn file_tag_is_four_bytes_even_when_size_byte_is_ascii_r() {
    let dir = TempDir::new();
    let mut fixture = Fixture::default();
    fixture.add("x", &[(0, b"a")]);
    let mut body = fixture.entries[0].body();
    body.extend(chunk(b"time", &[0; 10]));
    assert_eq!(body.len(), 0x72);
    let index = chunk(b"File", &body);
    assert_eq!(&index[..5], b"Filer");
    let bytes = with_index(&fixture.payload, &index, true);
    let archive = Xp3Archive::open(dir.write("test.xp3", &bytes)).unwrap();
    assert_eq!(archive.read("x").unwrap(), b"a");
}

#[test]
fn rejects_all_truncated_header_lengths_and_invalid_magic() {
    let bytes = one_file().bytes(true);
    for length in 0..19 {
        assert_open_error(&bytes[..length], "truncated XP3 header");
    }
    let mut wrong = bytes;
    wrong[0] = b'Y';
    assert_open_error(&wrong, "magic");
}

#[test]
fn rejects_out_of_bounds_and_overflowed_index_offsets() {
    for offset in [0, 18, 100_000, u64::MAX] {
        let mut bytes = one_file().bytes(true);
        bytes[11..19].copy_from_slice(&offset.to_le_bytes());
        assert!(opens(&bytes).is_err(), "accepted index offset {offset}");
    }
}

#[test]
fn rejects_index_flags_headers_zlib_and_output_length_errors() {
    let fixture = one_file();
    let bytes = fixture.bytes(true);
    let offset = 19 + fixture.payload.len();
    for flag in [2, 3, 0x40, 0xff] {
        let mut invalid = bytes.clone();
        invalid[offset] = flag;
        assert_open_error(&invalid, "unsupported XP3 index flag");
    }
    for end in offset..offset + 17 {
        assert!(
            opens(&bytes[..end]).is_err(),
            "accepted header truncation at {end}"
        );
    }
    let mut invalid = bytes.clone();
    invalid[offset + 17] ^= 0xff;
    assert_open_error(&invalid, "zlib");
    for difference in [-1_i64, 1] {
        let mut invalid = bytes.clone();
        let declared = (fixture.index().len() as i64 + difference) as u64;
        invalid[offset + 9..offset + 17].copy_from_slice(&declared.to_le_bytes());
        assert_open_error(
            &invalid,
            if difference < 0 {
                "exceeds declared"
            } else {
                "size mismatch"
            },
        );
    }
    let mut truncated = bytes.clone();
    truncated.pop();
    assert_open_error(&truncated, "range");
    // Claim a shorter stored stream so its missing zlib trailer is in bounds.
    let mut truncated = bytes.clone();
    let stored = u64::from_le_bytes(truncated[offset + 1..offset + 9].try_into().unwrap());
    truncated[offset + 1..offset + 9].copy_from_slice(&(stored - 1).to_le_bytes());
    assert_open_error(&truncated, "zlib");
    let mut trailing = bytes.clone();
    trailing.push(0x42);
    trailing[offset + 1..offset + 9].copy_from_slice(&(stored + 1).to_le_bytes());
    assert_open_error(&trailing, "trailing bytes after zlib");
}

#[test]
fn rejects_malformed_chunk_lengths_and_top_level_tags() {
    let fixture = one_file();
    for index in [&b"File"[..], &b"File\x01\0\0\0\0\0\0\0"[..]] {
        assert_open_error(
            &with_index(&fixture.payload, index, false),
            "truncated XP3 chunk",
        );
    }
    let mut index = fixture.index();
    index[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(opens(&with_index(&fixture.payload, &index, false)).is_err());
    let mut index = fixture.index();
    index[..4].copy_from_slice(b"Fail");
    assert_open_error(
        &with_index(&fixture.payload, &index, false),
        "four-byte File",
    );
    let mut index = fixture.index();
    index[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(opens(&with_index(&fixture.payload, &index, false)).is_err());
}

#[test]
fn requires_unique_info_segm_adlr_and_exact_subchunk_lengths() {
    let fixture = one_file();
    let spec = &fixture.entries[0];
    let parts = [
        chunk(b"info", &spec.info()),
        chunk(b"segm", &spec.segm()),
        chunk(b"adlr", &spec.checksum.to_le_bytes()),
    ];
    for missing in 0..3 {
        let body: Vec<u8> = parts
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != missing)
            .flat_map(|(_, part)| part.clone())
            .collect();
        assert_open_error(
            &with_index(&fixture.payload, &chunk(b"File", &body), false),
            "missing",
        );
    }
    for duplicate in 0..3 {
        let mut body = spec.body();
        body.extend(&parts[duplicate]);
        assert_open_error(
            &with_index(&fixture.payload, &chunk(b"File", &body), true),
            "duplicate",
        );
    }
    for malformed in [chunk(b"adlr", &[0; 3]), chunk(b"adlr", &[0; 5])] {
        let mut body = parts[0].clone();
        body.extend(&parts[1]);
        body.extend(malformed);
        assert_open_error(
            &with_index(&fixture.payload, &chunk(b"File", &body), false),
            "exactly four",
        );
    }
    let mut info = spec.info();
    info.push(0);
    let mut body = chunk(b"info", &info);
    body.extend(&parts[1]);
    body.extend(&parts[2]);
    assert_open_error(
        &with_index(&fixture.payload, &chunk(b"File", &body), false),
        "trailing bytes",
    );
    let mut segm = spec.segm();
    segm.push(0);
    let mut body = parts[0].clone();
    body.extend(chunk(b"segm", &segm));
    body.extend(&parts[2]);
    assert_open_error(
        &with_index(&fixture.payload, &chunk(b"File", &body), false),
        "multiple of 28",
    );
}

#[test]
fn rejects_invalid_utf16_name_lengths_traversal_and_duplicate_paths() {
    let mut fixture = one_file();
    for name in ["", "../a", "a/../b", "/a", "C:\\x", "a\0b"] {
        fixture.entries[0].name = name.to_owned();
        assert!(opens(&fixture.bytes(false)).is_err(), "accepted {name:?}");
    }
    fixture.entries[0].name = "x".to_owned();
    let spec = &fixture.entries[0];
    for invalid in [vec![0x00, 0xd8], vec![0x00, 0xdc]] {
        let mut info = spec.info();
        info[22..24].copy_from_slice(&invalid);
        let mut body = chunk(b"info", &info);
        body.extend(chunk(b"segm", &spec.segm()));
        body.extend(chunk(b"adlr", &spec.checksum.to_le_bytes()));
        assert_open_error(
            &with_index(&fixture.payload, &chunk(b"File", &body), false),
            "invalid UTF-16",
        );
    }
    let mut info = spec.info();
    info[20..22].copy_from_slice(&100u16.to_le_bytes());
    let mut body = chunk(b"info", &info);
    body.extend(chunk(b"segm", &spec.segm()));
    body.extend(chunk(b"adlr", &spec.checksum.to_le_bytes()));
    assert_open_error(
        &with_index(&fixture.payload, &chunk(b"File", &body), false),
        "truncated",
    );
    fixture.alias(0, "./X");
    assert_open_error(&fixture.bytes(true), "duplicate normalized");
}

#[test]
fn rejects_unsupported_flags_size_mismatches_ranges_and_range_overflow() {
    let fixture = one_file();
    let reject =
        |changed: Fixture, expected: &str| assert_open_error(&changed.bytes(true), expected);
    let changed = || Fixture {
        payload: fixture.payload.clone(),
        entries: fixture.entries.clone(),
    };
    let mut f = changed();
    f.entries[0].flags = 0x80000000;
    reject(f, "info flags");
    let mut f = changed();
    f.entries[0].segments[0].flags = 2;
    reject(f, "segment flags");
    let mut f = changed();
    f.entries[0].segments[0].original += 1;
    reject(f, "raw XP3 segment");
    let mut f = changed();
    f.entries[0].original += 1;
    reject(f, "info/segment original");
    let mut f = changed();
    f.entries[0].stored += 1;
    reject(f, "info/segment stored");
    let mut f = changed();
    f.entries[0].segments[0].offset = u64::MAX;
    reject(f, "overflow");
    let mut f = changed();
    f.entries[0].segments[0].offset = 100_000;
    reject(f, "exceeds archive");
    let mut f = changed();
    f.entries[0].segments[0].offset = 0;
    reject(f, "overlaps header");
    let mut f = changed();
    f.entries[0].segments[0].offset = 19 + f.payload.len() as u64;
    reject(f, "overlaps index");
    let mut f = changed();
    f.entries[0].segments.clear();
    reject(f, "has no segments");
    let mut f = changed();
    f.entries[0].original = u64::MAX;
    reject(f, "output budget");
}

#[test]
fn detects_checksum_corruption_without_treating_adlr_as_encryption() {
    let dir = TempDir::new();
    let mut fixture = one_file();
    assert_ne!(fixture.entries[0].checksum, 0);
    let path = dir.write("test.xp3", &fixture.bytes(false));
    let archive = Xp3Archive::open(&path).unwrap();
    assert_eq!(archive.read("Dir/台詞.ks").unwrap(), b"plain bytes");
    fixture.payload[0] ^= 1;
    fs::write(&path, fixture.bytes(false)).unwrap();
    let error = archive.read("Dir/台詞.ks").unwrap_err();
    assert!(format!("{error:#}").contains("checksum mismatch"));
    fs::write(&path, [0; 19]).unwrap();
    assert!(format!("{:#}", archive.read("Dir/台詞.ks").unwrap_err()).contains("size changed"));
}

#[test]
fn validates_segment_zlib_exact_input_and_output_and_truncation() {
    for mode in ["short", "long", "truncated", "trailing", "invalid"] {
        let dir = TempDir::new();
        let mut fixture = Fixture::default();
        fixture.add("x", &[(1, b"the compressed payload")]);
        match mode {
            "short" => {
                fixture.entries[0].original -= 1;
                fixture.entries[0].segments[0].original -= 1;
            }
            "long" => {
                fixture.entries[0].original += 1;
                fixture.entries[0].segments[0].original += 1;
            }
            "truncated" => {
                fixture.payload.pop();
                fixture.entries[0].stored -= 1;
                fixture.entries[0].segments[0].stored -= 1;
            }
            "trailing" => {
                fixture.payload.push(0);
                fixture.entries[0].stored += 1;
                fixture.entries[0].segments[0].stored += 1;
            }
            "invalid" => fixture.payload[0] ^= 0xff,
            _ => unreachable!(),
        }
        let archive = Xp3Archive::open(dir.write("test.xp3", &fixture.bytes(false))).unwrap();
        let error = format!("{:#}", archive.read("x").unwrap_err());
        assert!(error.contains("zlib"), "{mode}: {error}");
    }
}

#[test]
fn supports_large_streaming_segment_outputs_without_chunk_boundary_errors() {
    let dir = TempDir::new();
    let original: Vec<u8> = (0..250_000u32)
        .map(|i| ((i.wrapping_mul(7) ^ (i >> 8)) & 255) as u8)
        .collect();
    let mut fixture = Fixture::default();
    fixture.add("large", &[(1, &original)]);
    let archive = Xp3Archive::open(dir.write("test.xp3", &fixture.bytes(true))).unwrap();
    assert_eq!(archive.read("large").unwrap(), original);
}

#[test]
fn enforces_cumulative_index_entry_segment_and_output_budgets() {
    let dir = TempDir::new();
    let mut fixture = one_file();
    fixture.add("b", &[(0, b"b")]);
    let path = dir.write("test.xp3", &fixture.bytes(true));
    let default = ArchiveLimits::default();
    for limits in [
        ArchiveLimits {
            max_index_bytes: 1,
            ..default
        },
        ArchiveLimits {
            max_stored_index_bytes: 1,
            ..default
        },
        ArchiveLimits {
            max_file_bytes: 1,
            ..default
        },
        ArchiveLimits {
            max_entries: 1,
            ..default
        },
        ArchiveLimits {
            max_segments: 1,
            ..default
        },
        ArchiveLimits {
            max_index_blocks: 0,
            ..default
        },
    ] {
        assert!(Xp3Archive::open_with_limits(&path, limits).is_err());
    }
}

#[test]
fn reads_chained_raw_and_zlib_indexes_and_rejects_cycles() {
    let dir = TempDir::new();
    let mut fixture = one_file();
    fixture.add("second", &[(1, b"chain")]);
    let first_offset = 19 + fixture.payload.len() as u64;
    let first_block = index_block(&fixture.entries[0].encode(), false, true);
    let second_offset = first_offset + first_block.len() as u64 + 8 + 7;
    let mut bytes = XP3_MAGIC.to_vec();
    bytes.extend_from_slice(&first_offset.to_le_bytes());
    bytes.extend(&fixture.payload);
    bytes.extend(&first_block);
    let pointer_position = bytes.len();
    bytes.extend_from_slice(&second_offset.to_le_bytes());
    bytes.extend([0xcc; 7]); // Deliberate gap: continuation must seek, not scan.
    bytes.extend(index_block(&fixture.entries[1].encode(), true, false));
    let path = dir.write("test.xp3", &bytes);
    let archive = Xp3Archive::open(&path).unwrap();
    assert_eq!(archive.stats().index_blocks, 2);
    assert_eq!(archive.read("second").unwrap(), b"chain");
    assert_eq!(archive.read("Dir/台詞.ks").unwrap(), b"plain bytes");
    let limits = ArchiveLimits {
        max_index_blocks: 1,
        ..ArchiveLimits::default()
    };
    assert!(Xp3Archive::open_with_limits(&path, limits).is_err());
    let limits = ArchiveLimits {
        max_index_bytes: fixture.index().len() as u64 - 1,
        ..ArchiveLimits::default()
    };
    assert!(Xp3Archive::open_with_limits(&path, limits).is_err());
    bytes[pointer_position..pointer_position + 8].copy_from_slice(&first_offset.to_le_bytes());
    assert_open_error(&bytes, "cyclic");
    bytes[pointer_position..pointer_position + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(opens(&bytes).is_err());
    assert!(opens(&bytes[..pointer_position + 7]).is_err());
}

#[test]
fn patch_wins_for_exact_basename_extensionless_and_different_prefix_aliases() {
    let dir = TempDir::new();
    let mut base = Fixture::default();
    base.add("scenario/Title.ks", &[(0, b"base")]);
    base.add("images/scene.webp", &[(0, b"base image")]);
    base.add("BaseOnly.txt", &[(0, b"base only")]);
    dir.write("data.xp3", &base.bytes(true));
    let mut patch = Fixture::default();
    patch.add("SCENARIO/title.KS", &[(1, b"patched")]);
    let image = patch.add("data/images/scene.webp", &[(0, b"patch image")]);
    patch.alias(image, "scene.webp");
    dir.write("patch.xp3", &patch.bytes(true));
    // An old extracted loose file must not win, change, or be read.
    let loose = dir.write("BaseOnly.txt", b"loose stale bytes");
    let store = AssetStore::open(dir.path()).unwrap();
    for name in [
        "scenario/title.ks",
        "./SCENARIO\\TITLE.KS",
        "title.ks",
        "title",
        "scenario/title",
    ] {
        assert_eq!(store.read(name).unwrap(), b"patched", "{name}");
    }
    for name in [
        "images/scene.webp",
        "scene.webp",
        "scene",
        "data/images/scene",
    ] {
        assert_eq!(store.read(name).unwrap(), b"patch image", "{name}");
    }
    assert_eq!(
        store.resolve("SCENARIO/title.ks").unwrap(),
        "SCENARIO/title.KS"
    );
    assert_eq!(store.resolve("scene").unwrap(), "data/images/scene.webp");
    assert!(store.read("scene.png").is_err());
    assert_eq!(store.read("baseonly.txt").unwrap(), b"base only");
    assert_eq!(fs::read(loose).unwrap(), b"loose stale bytes");
    assert_eq!(store.stats().archive_count, 2);
    assert_eq!(store.stats().source_entry_count, 6);
    assert_eq!(store.stats().entry_count, 5);
    let mut names = store.names();
    names.sort_by_key(|name| name.to_lowercase());
    assert_eq!(store.names(), names);
    assert_eq!(store.archives().len(), 2);
    assert_eq!(store.root(), dir.path().canonicalize().unwrap());
}

#[test]
fn three_layer_overlays_resolve_exact_extensionless_basename_and_prefixed_paths() {
    fn assert_reads(store: &AssetStore, names: &[&str], expected: &[u8]) {
        for name in names {
            assert_eq!(store.read(name).unwrap(), expected, "{name}");
            assert_eq!(
                store.read_optional(name).unwrap().as_deref(),
                Some(expected),
                "{name}"
            );
        }
    }

    let mut base = Fixture::default();
    base.add("scenario/Title.ks", &[(0, b"base exact")]);
    base.add("images/scene.webp", &[(0, b"base image")]);
    base.add("base-only.txt", &[(0, b"base only")]);
    let mut adult = Fixture::default();
    adult.add("SCENARIO/title.KS", &[(1, b"adult exact")]);
    adult.add("adult-prefix/images/scene.webp", &[(0, b"adult image")]);
    adult.add("adult-only.txt", &[(0, b"adult only")]);
    let mut patch = Fixture::default();
    patch.add("scenario/title.ks", &[(0, b"patch exact")]);
    patch.add("patch-prefix/images/scene.webp", &[(1, b"patch image")]);
    patch.add("patch-only.txt", &[(0, b"patch only")]);

    let title_queries = [
        "scenario/title.ks",
        "./SCENARIO\\TITLE.KS",
        "scenario/title",
        "title.ks",
        "title",
    ];
    let image_queries = [
        "images/scene.webp",
        "images/scene",
        "scene.webp",
        "scene",
        "adult-prefix/images/scene.webp",
        "adult-prefix/images/scene",
        "patch-prefix/images/scene.webp",
        "patch-prefix/images/scene",
    ];
    for include_adult in [false, true] {
        for include_patch in [false, true] {
            let dir = TempDir::new();
            dir.write("data.xp3", &base.bytes(true));
            let mut expected_archives = vec!["data.xp3"];
            if include_adult {
                dir.write("adult.xp3", &adult.bytes(false));
                expected_archives.push("adult.xp3");
            }
            if include_patch {
                dir.write("patch.xp3", &patch.bytes(true));
                expected_archives.push("patch.xp3");
            }
            let store = AssetStore::open(dir.path()).unwrap();
            let (title, image, title_path, image_path): (&[u8], &[u8], &str, &str) =
                if include_patch {
                    (
                        b"patch exact",
                        b"patch image",
                        "scenario/title.ks",
                        "patch-prefix/images/scene.webp",
                    )
                } else if include_adult {
                    (
                        b"adult exact",
                        b"adult image",
                        "SCENARIO/title.KS",
                        "adult-prefix/images/scene.webp",
                    )
                } else {
                    (
                        b"base exact",
                        b"base image",
                        "scenario/Title.ks",
                        "images/scene.webp",
                    )
                };
            assert_reads(&store, &title_queries, title);
            assert_reads(&store, &image_queries, image);
            assert_eq!(store.resolve("scenario/title.ks").unwrap(), title_path);
            assert_eq!(store.resolve("images/scene.webp").unwrap(), image_path);
            assert_reads(&store, &["base-only.txt", "base-only"], b"base only");
            for (present, name, bytes) in [
                (include_adult, "adult-only", &b"adult only"[..]),
                (include_patch, "patch-only", &b"patch only"[..]),
            ] {
                for query in [name.to_owned(), format!("{name}.txt")] {
                    assert_eq!(
                        store.read_optional(&query).unwrap().as_deref(),
                        present.then_some(bytes),
                        "{query}: adult={include_adult}, patch={include_patch}"
                    );
                    if !present {
                        assert!(store.read(&query).is_err());
                    }
                }
            }
            assert_eq!(store.stats().archive_count, expected_archives.len());
            assert_eq!(
                store.stats().source_entry_count,
                3 * expected_archives.len()
            );
            assert_eq!(
                store
                    .archives()
                    .iter()
                    .map(|archive| archive.path().file_name().unwrap().to_str().unwrap())
                    .collect::<Vec<_>>(),
                expected_archives
            );

            let base_store = AssetStore::open_base(dir.path()).unwrap();
            assert_reads(
                &base_store,
                &title_queries,
                if include_patch {
                    b"patch exact"
                } else {
                    b"base exact"
                },
            );
            assert_reads(
                &base_store,
                &image_queries,
                if include_patch {
                    b"patch image"
                } else {
                    b"base image"
                },
            );
            assert_reads(&base_store, &["base-only"], b"base only");
            assert_eq!(base_store.read_optional("adult-only").unwrap(), None);
            assert_eq!(
                base_store.read_optional("patch-only").unwrap().as_deref(),
                include_patch.then_some(&b"patch only"[..])
            );
            assert_eq!(
                base_store.stats().archive_count,
                1 + usize::from(include_patch)
            );
        }
    }
}

#[test]
fn optional_archives_reject_corruption_and_directories_but_base_ignores_adult() {
    let mut base = Fixture::default();
    base.add("shared.txt", &[(0, b"base")]);
    base.add("base-only.txt", &[(0, b"base only")]);
    let mut adult = Fixture::default();
    adult.add("shared.txt", &[(0, b"adult")]);
    let mut patch = Fixture::default();
    patch.add("shared.txt", &[(0, b"patch")]);
    for archive_name in ["adult.xp3", "patch.xp3"] {
        for invalid_kind in ["corrupt", "directory"] {
            let dir = TempDir::new();
            dir.write("data.xp3", &base.bytes(true));
            if archive_name == "adult.xp3" {
                dir.write("patch.xp3", &patch.bytes(true));
            } else {
                dir.write("adult.xp3", &adult.bytes(true));
            }
            let old_base_fingerprint = AssetStore::open_base(dir.path())
                .unwrap()
                .fingerprint()
                .to_owned();
            if invalid_kind == "directory" {
                fs::create_dir(dir.path().join(archive_name)).unwrap();
            } else {
                dir.write(archive_name, b"not a valid synthetic XP3 archive");
            }
            let assert_diagnostic = |error: anyhow::Error| {
                let message = format!("{error:#}");
                assert!(
                    message.contains(archive_name),
                    "{archive_name} ({invalid_kind}): {message}"
                );
                let expected = if invalid_kind == "directory" {
                    "not a regular file"
                } else {
                    "invalid XP3 magic"
                };
                assert!(
                    message.contains(expected),
                    "expected {expected:?}: {message}"
                );
            };
            assert_diagnostic(AssetStore::open(dir.path()).unwrap_err());
            if archive_name == "adult.xp3" {
                let base_store = AssetStore::open_base(dir.path()).unwrap();
                assert_eq!(base_store.read("shared").unwrap(), b"patch");
                assert_eq!(base_store.read("base-only").unwrap(), b"base only");
                assert_eq!(base_store.stats().archive_count, 2);
                assert_eq!(base_store.fingerprint(), old_base_fingerprint);
            } else {
                // Base compatibility mode skips adult, never a malformed patch.
                assert_diagnostic(AssetStore::open_base(dir.path()).unwrap_err());
            }
        }
    }
}

#[test]
fn higher_layer_alias_ambiguity_never_falls_back_to_lower_layers() {
    for high_archive in ["adult.xp3", "patch.xp3"] {
        let dir = TempDir::new();
        let mut lower = Fixture::default();
        lower.add("base/shared.ks", &[(0, b"lower shared")]);
        lower.add("stems/twin", &[(0, b"lower stem")]);
        dir.write("data.xp3", &lower.bytes(false));
        if high_archive == "patch.xp3" {
            dir.write("adult.xp3", &lower.bytes(true));
        } else {
            let mut unrelated_patch = Fixture::default();
            unrelated_patch.add("unrelated.txt", &[(0, b"unrelated patch")]);
            dir.write("patch.xp3", &unrelated_patch.bytes(true));
        }
        let lower_store = AssetStore::open(dir.path()).unwrap();
        for query in ["base/shared.ks", "base/shared", "shared.ks", "shared"] {
            assert_eq!(lower_store.read(query).unwrap(), b"lower shared");
        }
        assert_eq!(lower_store.read("stems/twin").unwrap(), b"lower stem");

        let mut ambiguous = Fixture::default();
        // Equal content/checksums at distinct offsets are not physical aliases.
        for name in [
            "z/shared.ks",
            "a/shared.ks",
            "stems/twin.ks",
            "stems/twin.tjs",
        ] {
            ambiguous.add(name, &[(0, b"same")]);
        }
        dir.write(high_archive, &ambiguous.bytes(true));
        let store = AssetStore::open(dir.path()).unwrap();
        for (query, candidates) in [
            ("base/shared.ks", ["a/shared.ks", "z/shared.ks"]),
            ("base/shared", ["a/shared.ks", "z/shared.ks"]),
            ("shared.ks", ["a/shared.ks", "z/shared.ks"]),
            ("shared", ["a/shared.ks", "z/shared.ks"]),
            ("stems/twin", ["stems/twin.ks", "stems/twin.tjs"]),
        ] {
            for message in [
                format!("{:#}", store.resolve(query).unwrap_err()),
                format!("{:#}", store.read(query).unwrap_err()),
                format!("{:#}", store.resolve_optional(query).unwrap_err()),
                format!("{:#}", store.read_optional(query).unwrap_err()),
            ] {
                assert!(message.contains("ambiguous asset alias"), "{message}");
                assert!(message.contains(high_archive), "{message}");
                let first = message.find(candidates[0]).unwrap();
                let second = message.find(candidates[1]).unwrap();
                assert!(first < second, "{message}");
            }
        }
        for name in [
            "a/shared.ks",
            "z/shared.ks",
            "stems/twin.ks",
            "stems/twin.tjs",
        ] {
            assert_eq!(store.read(name).unwrap(), b"same");
        }
        if high_archive == "adult.xp3" {
            let base_store = AssetStore::open_base(dir.path()).unwrap();
            assert_eq!(base_store.read("base/shared.ks").unwrap(), b"lower shared");
            assert_eq!(base_store.read("stems/twin").unwrap(), b"lower stem");
        }
    }
}

#[test]
fn base_mode_keeps_legacy_config_fingerprint_when_optional_overlay_is_added() {
    let dir = TempDir::new();
    let relocated = TempDir::new();
    let mut base = Fixture::default();
    base.add("shared.txt", &[(0, b"base")]);
    let mut adult = Fixture::default();
    adult.add("shared.txt", &[(1, b"adult")]);
    let mut patch = Fixture::default();
    patch.add("shared.txt", &[(0, b"patch")]);
    dir.write("data.xp3", &base.bytes(true));
    let data_only_fingerprint = AssetStore::open(dir.path())
        .unwrap()
        .fingerprint()
        .to_owned();
    assert_eq!(
        AssetStore::open_base(dir.path()).unwrap().fingerprint(),
        data_only_fingerprint
    );

    dir.write("patch.xp3", &patch.bytes(true));
    let legacy_store = AssetStore::open(dir.path()).unwrap();
    // Preserve the identity recorded by an existing data+patch configuration.
    let legacy_config_fingerprint = legacy_store.fingerprint().to_owned();
    assert_ne!(legacy_config_fingerprint, data_only_fingerprint);
    assert_eq!(
        AssetStore::open_base(dir.path()).unwrap().fingerprint(),
        legacy_config_fingerprint
    );

    dir.write("adult.xp3", &adult.bytes(true));
    let full_store = AssetStore::open(dir.path()).unwrap();
    assert_ne!(full_store.fingerprint(), legacy_config_fingerprint);
    // The added layer is entirely shadowed: fingerprint must track archives,
    // not just the effective names or bytes that resolve from the patch.
    assert_eq!(full_store.names(), legacy_store.names());
    assert_eq!(full_store.read("shared").unwrap(), b"patch");
    let base_store = AssetStore::open_base(dir.path()).unwrap();
    assert_eq!(base_store.fingerprint(), legacy_config_fingerprint);
    assert_eq!(base_store.names(), legacy_store.names());
    assert_eq!(base_store.read("shared").unwrap(), b"patch");
    assert_eq!(full_store.stats().archive_count, 3);
    assert_eq!(base_store.stats().archive_count, 2);

    // The same old configuration remains compatible after relocation too.
    relocated.write("data.xp3", &base.bytes(true));
    relocated.write("adult.xp3", &adult.bytes(true));
    relocated.write("patch.xp3", &patch.bytes(true));
    assert_eq!(
        AssetStore::open_base(relocated.path())
            .unwrap()
            .fingerprint(),
        legacy_config_fingerprint
    );
    assert_eq!(
        AssetStore::open(relocated.path()).unwrap().fingerprint(),
        full_store.fingerprint()
    );
}

#[test]
fn ambiguity_is_explicit_sorted_and_not_resolved_by_equal_checksums() {
    let dir = TempDir::new();
    let mut base = Fixture::default();
    base.add("z/Shared.ks", &[(0, b"same")]);
    base.add("a/Shared.ks", &[(0, b"same")]);
    base.add("z/Shared.tjs", &[(0, b"other extension")]);
    dir.write("data.xp3", &base.bytes(false));
    let store = AssetStore::open(dir.path()).unwrap();
    let error = format!("{:#}", store.resolve("shared.ks").unwrap_err());
    assert!(error.contains("ambiguous asset alias"));
    assert!(error.find("a/Shared.ks").unwrap() < error.find("z/Shared.ks").unwrap());
    assert_eq!(store.read("z/shared.ks").unwrap(), b"same");
    assert_eq!(store.resolve("a/shared").unwrap(), "a/Shared.ks");
    assert!(store.resolve("shared").is_err());
    assert!(
        store.resolve("z/shared").is_err(),
        "explicit extension required for colliding stems"
    );
    assert!(store.resolve("../shared").is_err());
}

#[test]
fn ambiguous_patch_does_not_fall_back_to_base_exact_path() {
    let dir = TempDir::new();
    let mut base = Fixture::default();
    base.add("base/x.ks", &[(0, b"base")]);
    dir.write("data.xp3", &base.bytes(false));
    let mut patch = Fixture::default();
    patch.add("a/x.ks", &[(0, b"one")]);
    patch.add("b/x.ks", &[(0, b"two")]);
    dir.write("patch.xp3", &patch.bytes(false));
    let store = AssetStore::open(dir.path()).unwrap();
    assert!(format!("{:#}", store.read("base/x.ks").unwrap_err()).contains("ambiguous"));
}

#[test]
fn missing_patch_is_allowed_but_malformed_patch_is_an_error() {
    let dir = TempDir::new();
    assert!(AssetStore::open(dir.path()).is_err());
    dir.write("data.xp3", &one_file().bytes(true));
    assert_eq!(
        AssetStore::open(dir.path()).unwrap().stats().archive_count,
        1
    );
    dir.write("patch.xp3", b"not XP3");
    assert!(AssetStore::open(dir.path()).is_err());
}

#[test]
fn overlay_text_decodes_bom_and_cp932_and_fingerprint_is_relocatable() {
    let dir = TempDir::new();
    let second = TempDir::new();
    let mut fixture = Fixture::default();
    fixture.add("utf8.ks", &[(1, "\u{feff}日本語".as_bytes())]);
    fixture.add(
        "utf16.ks",
        &[(0, &[0xff, 0xfe, 0xe5, 0x65, 0x2c, 0x67, 0x9e, 0x8a])],
    );
    fixture.add("cp932.ks", &[(0, &[0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea])]);
    let bytes = fixture.bytes(true);
    dir.write("data.xp3", &bytes);
    second.write("data.xp3", &bytes);
    let store = AssetStore::open(dir.path()).unwrap();
    for name in ["utf8", "utf16", "cp932"] {
        assert_eq!(store.text(name).unwrap(), "日本語");
    }
    assert_eq!(
        store.fingerprint(),
        AssetStore::open(second.path()).unwrap().fingerprint()
    );
    fixture.entries[0].checksum ^= 1;
    second.write("data.xp3", &fixture.bytes(true));
    assert_ne!(
        store.fingerprint(),
        AssetStore::open(second.path()).unwrap().fingerprint()
    );
}

#[test]
fn immutable_archives_and_stores_allow_parallel_readers() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Xp3Archive>();
    send_sync::<AssetStore>();
    let dir = TempDir::new();
    let mut fixture = Fixture::default();
    let expected = "parallel:日本語".repeat(4096);
    fixture.add("script.ks", &[(1, expected.as_bytes())]);
    dir.write("data.xp3", &fixture.bytes(true));
    let store = Arc::new(AssetStore::open(dir.path()).unwrap());
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            let expected = expected.clone();
            std::thread::spawn(move || {
                for _ in 0..8 {
                    assert_eq!(store.text("script").unwrap(), expected);
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}
