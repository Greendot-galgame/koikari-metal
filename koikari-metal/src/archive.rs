//! Read-only, bounded reader for ordinary (unfiltered) Kirikiri XP3 archives.
//!
//! Opening reads the 19-byte header and the index blocks, never scans payloads.
//! Every asset read opens an independent file handle, seeks to each segment,
//! reads exactly its stored bytes, validates zlib framing/output lengths, and
//! verifies the `adlr` checksum of the concatenated original bytes. `adlr` is
//! not interpreted as encryption; unsupported `info`/segment flags fail closed.

use anyhow::{Context, Result, bail, ensure};
use flate2::{Decompress, FlushDecompress, Status};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const XP3_MAGIC: &[u8; 11] = b"XP3\r\n \n\x1a\x8bg\x01";
const HEADER_BYTES: u64 = 19;
const IO_CHUNK: usize = 32 * 1024;

/// Allocation/work limits. Index byte budgets are cumulative across chains.
/// File budgets apply to both the declared original and stored total sizes.
#[derive(Debug, Clone, Copy)]
pub struct ArchiveLimits {
    pub max_index_bytes: u64,
    pub max_stored_index_bytes: u64,
    pub max_file_bytes: u64,
    pub max_entries: usize,
    pub max_segments: usize,
    pub max_index_blocks: usize,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_index_bytes: 64 * 1024 * 1024,
            max_stored_index_bytes: 64 * 1024 * 1024,
            max_file_bytes: 512 * 1024 * 1024,
            max_entries: 250_000,
            max_segments: 1_000_000,
            max_index_blocks: 128,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Segment {
    pub flags: u32,
    /// Absolute file offset, NOT an original or stored size.
    pub offset: u64,
    pub original_size: u64,
    pub stored_size: u64,
}

impl Segment {
    pub fn is_compressed(&self) -> bool {
        self.flags == 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveEntry {
    /// Case-preserved name with `/` separators and redundant `.` removed.
    pub name: String,
    pub flags: u32,
    pub original_size: u64,
    pub stored_size: u64,
    pub adler32: u32,
    pub segments: Vec<Segment>,
}

impl ArchiveEntry {
    /// Proves duplicate names refer to exactly the same physical bytes. A mere
    /// matching Adler-32 is deliberately insufficient to coalesce aliases.
    pub(crate) fn same_storage(&self, other: &Self) -> bool {
        self.flags == other.flags
            && self.original_size == other.original_size
            && self.stored_size == other.stored_size
            && self.adler32 == other.adler32
            && self.segments == other.segments
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ArchiveStats {
    pub archive_bytes: u64,
    pub entry_count: usize,
    pub segment_count: usize,
    pub compressed_segment_count: usize,
    /// Logical totals; physical segments shared by aliases count per entry.
    pub original_bytes: u64,
    pub stored_bytes: u64,
    pub index_blocks: usize,
    pub index_stored_bytes: u64,
    pub index_original_bytes: u64,
}

/// Immutable metadata, without a shared cursor; therefore naturally Send+Sync.
#[derive(Debug)]
pub struct Xp3Archive {
    path: PathBuf,
    entries: BTreeMap<String, ArchiveEntry>,
    stats: ArchiveStats,
    limits: ArchiveLimits,
    fingerprint: String,
}

impl Xp3Archive {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_limits(path, ArchiveLimits::default())
    }

    pub fn open_with_limits(path: impl AsRef<Path>, limits: ArchiveLimits) -> Result<Self> {
        let path = path
            .as_ref()
            .canonicalize()
            .with_context(|| format!("locate XP3 archive {}", path.as_ref().display()))?;
        let mut file =
            File::open(&path).with_context(|| format!("open XP3 archive {}", path.display()))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file(),
            "XP3 archive is not a regular file: {}",
            path.display()
        );
        let file_len = metadata.len();
        ensure!(
            file_len >= HEADER_BYTES,
            "truncated XP3 header in {}",
            path.display()
        );
        let mut header = [0u8; HEADER_BYTES as usize];
        file.read_exact(&mut header).context("read XP3 header")?;
        ensure!(
            &header[..11] == XP3_MAGIC,
            "invalid XP3 magic in {}",
            path.display()
        );
        let mut index_offset = u64::from_le_bytes(header[11..19].try_into()?);
        let mut seen = BTreeSet::new();
        let mut index_ranges = Vec::new();
        let mut index = Vec::new();
        let mut stats = ArchiveStats {
            archive_bytes: file_len,
            ..ArchiveStats::default()
        };
        let mut fingerprint = Fingerprint::new();
        fingerprint.bytes(b"ordinary-xp3-index-v1");
        fingerprint.bytes(&file_len.to_le_bytes());

        loop {
            ensure!(
                stats.index_blocks < limits.max_index_blocks,
                "XP3 index block budget exceeded"
            );
            ensure!(
                index_offset >= HEADER_BYTES,
                "XP3 index offset overlaps header"
            );
            ensure!(
                seen.insert(index_offset),
                "cyclic XP3 index chain at {index_offset}"
            );
            check_range(index_offset, 1, file_len, "index flag")?;
            file.seek(SeekFrom::Start(index_offset))?;
            let flag = read_u8(&mut file)?;
            ensure!(flag & !0x81 == 0, "unsupported XP3 index flag {flag:#04x}");
            let compressed = flag & 1 != 0;
            let size_header = if compressed { 17 } else { 9 };
            check_range(index_offset, size_header, file_len, "index header")?;
            let stored = read_u64(&mut file)?;
            let original = if compressed {
                read_u64(&mut file)?
            } else {
                stored
            };
            stats.index_stored_bytes =
                checked_sum(stats.index_stored_bytes, stored, "stored index size")?;
            stats.index_original_bytes =
                checked_sum(stats.index_original_bytes, original, "original index size")?;
            ensure!(
                stats.index_stored_bytes <= limits.max_stored_index_bytes,
                "XP3 stored index byte budget exceeded"
            );
            ensure!(
                stats.index_original_bytes <= limits.max_index_bytes,
                "XP3 decoded index byte budget exceeded"
            );
            let payload_offset = checked_sum(index_offset, size_header, "index payload offset")?;
            let payload_end = check_range(payload_offset, stored, file_len, "index payload")?;
            let block_end = if flag & 0x80 != 0 {
                check_range(payload_end, 8, file_len, "index continuation pointer")?
            } else {
                payload_end
            };
            for &(start, end) in &index_ranges {
                ensure!(
                    block_end <= start || index_offset >= end,
                    "overlapping XP3 index blocks"
                );
            }
            index_ranges.push((index_offset, block_end));
            let decoded = if compressed {
                inflate_exact(&mut file, stored, original).context("decode XP3 zlib index")?
            } else {
                read_exact_vec(&mut file, stored).context("read raw XP3 index")?
            };
            index
                .try_reserve(decoded.len())
                .context("allocate XP3 index")?;
            index.extend_from_slice(&decoded);
            fingerprint.bytes(&index_offset.to_le_bytes());
            fingerprint.bytes(&[flag]);
            fingerprint.bytes(&stored.to_le_bytes());
            fingerprint.bytes(&original.to_le_bytes());
            fingerprint.bytes(&decoded);
            stats.index_blocks += 1;
            if flag & 0x80 == 0 {
                break;
            }
            // Standard CONTINUE layout: the following LE u64 is another
            // header-relative index offset. Our supported header starts at 0.
            // See docs.rs/xp3-brute/latest/src/xp3_brute/xp3_meta.rs.html#131.
            file.seek(SeekFrom::Start(payload_end))?;
            index_offset = read_u64(&mut file).context("read XP3 index continuation")?;
        }

        let entries = parse_index(&index, file_len, &index_ranges, limits, &mut stats)
            .with_context(|| format!("parse XP3 index in {}", path.display()))?;
        Ok(Self {
            path,
            entries,
            stats,
            limits,
            fingerprint: format!("xp3-index-v1-{:016x}", fingerprint.finish()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn stats(&self) -> &ArchiveStats {
        &self.stats
    }
    pub fn limits(&self) -> ArchiveLimits {
        self.limits
    }

    /// Stable, root-independent index/size identity, NOT a cryptographic hash
    /// of all payloads. Computing it does not scan a multi-gigabyte archive.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Keys are normalized lowercase paths, values preserve displayed case.
    pub fn entries(&self) -> &BTreeMap<String, ArchiveEntry> {
        &self.entries
    }

    pub fn names(&self) -> Vec<String> {
        self.entries.values().map(|e| e.name.clone()).collect()
    }

    pub fn entry(&self, name: &str) -> Result<Option<&ArchiveEntry>> {
        Ok(self.entries.get(&name_key(name)?))
    }

    /// Exact case-insensitive path lookup; aliases belong to AssetStore.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let key = name_key(name)?;
        self.read_key(&key)
    }

    pub(crate) fn read_key(&self, key: &str) -> Result<Vec<u8>> {
        let entry = self
            .entries
            .get(key)
            .with_context(|| format!("asset {key:?} not found in {}", self.path.display()))?;
        ensure!(
            entry.original_size <= self.limits.max_file_bytes,
            "XP3 file output budget exceeded"
        );
        let mut file = File::open(&self.path)
            .with_context(|| format!("reopen XP3 archive {}", self.path.display()))?;
        ensure!(
            file.metadata()?.len() == self.stats.archive_bytes,
            "XP3 archive size changed since opening"
        );
        let mut result = Vec::new();
        for (i, segment) in entry.segments.iter().enumerate() {
            file.seek(SeekFrom::Start(segment.offset))?;
            let bytes = if segment.is_compressed() {
                inflate_exact(&mut file, segment.stored_size, segment.original_size)
            } else {
                read_exact_vec(&mut file, segment.stored_size)
            }
            .with_context(|| format!("read segment {i} of {:?}", entry.name))?;
            let next = checked_sum(result.len() as u64, bytes.len() as u64, "file output size")?;
            ensure!(
                next <= entry.original_size && next <= self.limits.max_file_bytes,
                "XP3 file output budget exceeded"
            );
            if result.is_empty() {
                // The usual single-segment file needs no second full-size
                // allocation (important for the 253 MB opening movie).
                result = bytes;
            } else {
                result
                    .try_reserve(bytes.len())
                    .context("allocate XP3 file output")?;
                result.extend_from_slice(&bytes);
            }
        }
        ensure!(
            result.len() as u64 == entry.original_size,
            "XP3 original size mismatch for {:?}",
            entry.name
        );
        let checksum = adler32(&result);
        ensure!(
            checksum == entry.adler32,
            "XP3 Adler-32 checksum mismatch for {:?}: expected {:08x}, got {checksum:08x}",
            entry.name,
            entry.adler32
        );
        Ok(result)
    }
}

fn parse_index(
    index: &[u8],
    file_len: u64,
    index_ranges: &[(u64, u64)],
    limits: ArchiveLimits,
    stats: &mut ArchiveStats,
) -> Result<BTreeMap<String, ArchiveEntry>> {
    let mut entries = BTreeMap::new();
    let mut cursor = Cursor::new(index);
    while !cursor.is_empty() {
        let (tag, body) = cursor.chunk().context("read File chunk")?;
        ensure!(&tag == b"File", "expected four-byte File tag, got {tag:?}");
        ensure!(
            entries.len() < limits.max_entries,
            "XP3 entry count budget exceeded"
        );
        let entry = parse_entry(body, file_len, index_ranges, limits, stats)?;
        let key = name_key(&entry.name)?;
        ensure!(
            !entries.contains_key(&key),
            "duplicate normalized XP3 path {:?}",
            entry.name
        );
        stats.original_bytes = checked_sum(
            stats.original_bytes,
            entry.original_size,
            "archive original total",
        )?;
        stats.stored_bytes = checked_sum(
            stats.stored_bytes,
            entry.stored_size,
            "archive stored total",
        )?;
        entries.insert(key, entry);
    }
    stats.entry_count = entries.len();
    Ok(entries)
}

fn parse_entry(
    body: &[u8],
    file_len: u64,
    index_ranges: &[(u64, u64)],
    limits: ArchiveLimits,
    stats: &mut ArchiveStats,
) -> Result<ArchiveEntry> {
    let mut chunks = Cursor::new(body);
    let mut info = None;
    let mut segments = None;
    let mut checksum = None;
    while !chunks.is_empty() {
        let (tag, bytes) = chunks.chunk().context("read File subchunk")?;
        match &tag {
            b"info" => {
                ensure!(info.is_none(), "duplicate XP3 info chunk");
                let mut data = Cursor::new(bytes);
                let flags = data.u32()?;
                ensure!(
                    flags == 0,
                    "unsupported XP3 info flags {flags:#010x}; filtered/encrypted files are not supported"
                );
                let original = data.u64()?;
                let stored = data.u64()?;
                ensure!(
                    original <= limits.max_file_bytes,
                    "XP3 declared file output budget exceeded"
                );
                ensure!(
                    stored <= limits.max_file_bytes,
                    "XP3 declared stored file budget exceeded"
                );
                let units = usize::from(data.u16()?);
                let name_bytes =
                    data.take(units.checked_mul(2).context("UTF-16 name size overflow")?)?;
                ensure!(data.is_empty(), "trailing bytes in XP3 info chunk");
                let utf16: Vec<u16> = name_bytes
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect();
                let name = String::from_utf16(&utf16).context("invalid UTF-16 XP3 filename")?;
                info = Some((normalize_name(&name)?, flags, original, stored));
            }
            b"segm" => {
                ensure!(segments.is_none(), "duplicate XP3 segm chunk");
                ensure!(
                    bytes.len() % 28 == 0,
                    "XP3 segm size is not a multiple of 28"
                );
                let count = bytes.len() / 28;
                let total = stats
                    .segment_count
                    .checked_add(count)
                    .context("segment count overflow")?;
                ensure!(
                    total <= limits.max_segments,
                    "XP3 segment count budget exceeded"
                );
                stats.segment_count = total;
                let mut parsed = Vec::new();
                parsed
                    .try_reserve(count)
                    .context("allocate XP3 segment metadata")?;
                let mut data = Cursor::new(bytes);
                while !data.is_empty() {
                    let flags = data.u32()?;
                    ensure!(flags <= 1, "unsupported XP3 segment flags {flags:#010x}");
                    let offset = data.u64()?;
                    let original_size = data.u64()?;
                    let stored_size = data.u64()?;
                    ensure!(
                        original_size <= limits.max_file_bytes
                            && stored_size <= limits.max_file_bytes,
                        "XP3 segment byte budget exceeded"
                    );
                    let end = check_range(offset, stored_size, file_len, "segment")?;
                    ensure!(offset >= HEADER_BYTES, "XP3 segment overlaps header");
                    for &(start, index_end) in index_ranges {
                        ensure!(
                            stored_size == 0 || end <= start || offset >= index_end,
                            "XP3 segment overlaps index"
                        );
                    }
                    if flags == 0 {
                        ensure!(
                            original_size == stored_size,
                            "raw XP3 segment original/stored sizes differ"
                        );
                    } else {
                        stats.compressed_segment_count += 1;
                    }
                    parsed.push(Segment {
                        flags,
                        offset,
                        original_size,
                        stored_size,
                    });
                }
                segments = Some(parsed);
            }
            b"adlr" => {
                ensure!(checksum.is_none(), "duplicate XP3 adlr chunk");
                ensure!(
                    bytes.len() == 4,
                    "XP3 adlr chunk must contain exactly four bytes"
                );
                checksum = Some(u32::from_le_bytes(bytes.try_into()?));
            }
            // Standard optional metadata (e.g. time) can be skipped because
            // the enclosing chunk length has already been checked exactly.
            _ => {}
        }
    }
    let (name, flags, original_size, stored_size) = info.context("missing XP3 info chunk")?;
    let segments = segments.context("missing XP3 segm chunk")?;
    let adler32 = checksum.context("missing XP3 adlr chunk")?;
    ensure!(
        !segments.is_empty() || original_size == 0 && stored_size == 0,
        "nonempty XP3 file has no segments"
    );
    let mut segment_original = 0;
    let mut segment_stored = 0;
    for segment in &segments {
        segment_original = checked_sum(
            segment_original,
            segment.original_size,
            "segment original sum",
        )?;
        segment_stored = checked_sum(segment_stored, segment.stored_size, "segment stored sum")?;
    }
    ensure!(
        segment_original == original_size,
        "XP3 info/segment original size mismatch for {name:?}"
    );
    ensure!(
        segment_stored == stored_size,
        "XP3 info/segment stored size mismatch for {name:?}"
    );
    Ok(ArchiveEntry {
        name,
        flags,
        original_size,
        stored_size,
        adler32,
        segments,
    })
}

/// Ordinary archive names, not OS filesystem paths. Reject traversal, absolute
/// paths, NULs and Windows drive/stream syntax instead of sanitizing silently.
pub(crate) fn normalize_name(name: &str) -> Result<String> {
    ensure!(!name.is_empty(), "empty asset name");
    ensure!(
        !name.starts_with(['/', '\\']),
        "absolute asset name is not allowed"
    );
    ensure!(!name.contains(['\0', ':']), "invalid asset name");
    let replaced = name.replace('\\', "/");
    let mut parts = Vec::new();
    for part in replaced.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("asset path traversal is not allowed"),
            _ => parts.push(part),
        }
    }
    ensure!(!parts.is_empty(), "empty normalized asset name");
    Ok(parts.join("/"))
}

pub(crate) fn name_key(name: &str) -> Result<String> {
    Ok(normalize_name(name)?.to_lowercase())
}

fn checked_sum(left: u64, right: u64, what: &str) -> Result<u64> {
    left.checked_add(right)
        .with_context(|| format!("XP3 {what} overflow"))
}

fn check_range(offset: u64, size: u64, file_len: u64, what: &str) -> Result<u64> {
    let end = checked_sum(offset, size, what)?;
    ensure!(
        end <= file_len,
        "XP3 {what} range {offset}..{end} exceeds archive size {file_len}"
    );
    Ok(end)
}

fn read_u8(reader: &mut impl Read) -> Result<u8> {
    let mut bytes = [0; 1];
    reader
        .read_exact(&mut bytes)
        .context("truncated XP3 byte")?;
    Ok(bytes[0])
}

fn read_u64(reader: &mut impl Read) -> Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes).context("truncated XP3 u64")?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_exact_vec(reader: &mut impl Read, size: u64) -> Result<Vec<u8>> {
    let size = usize::try_from(size).context("XP3 size cannot fit address space")?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .context("allocate XP3 stored bytes")?;
    bytes.resize(size, 0);
    reader
        .read_exact(&mut bytes)
        .context("truncated XP3 stored bytes")?;
    Ok(bytes)
}

/// Low-level decompression is intentional: reader adapters can read ahead and
/// conceal trailing data or truncated streams. Require StreamEnd, exact input
/// consumption, and an exact bounded output length, including empty streams.
fn inflate_exact(reader: &mut impl Read, stored: u64, original: u64) -> Result<Vec<u8>> {
    let expected = usize::try_from(original).context("zlib output cannot fit address space")?;
    let mut decoder = Decompress::new(true);
    let mut input = [0u8; IO_CHUNK];
    let mut output = [0u8; IO_CHUNK];
    let mut remaining = stored;
    let mut input_len = 0;
    let mut input_pos = 0;
    let mut result = Vec::new();
    loop {
        if input_pos == input_len && remaining > 0 {
            input_len = remaining.min(IO_CHUNK as u64) as usize;
            reader
                .read_exact(&mut input[..input_len])
                .context("truncated zlib stored bytes")?;
            remaining -= input_len as u64;
            input_pos = 0;
        }
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        // `Finish` must only be requested once every stored byte has been handed
        // to the decoder AND the current input buffer is drained. miniz_oxide
        // reports a bogus stream error when `Finish` is passed while the output
        // buffer is still too small to hold the remainder of the stream.
        let flush = if remaining == 0 && input_pos == input_len {
            FlushDecompress::Finish
        } else {
            FlushDecompress::None
        };
        let status = decoder
            .decompress(&input[input_pos..input_len], &mut output, flush)
            .context("invalid zlib stream")?;
        let consumed = (decoder.total_in() - before_in) as usize;
        let produced = (decoder.total_out() - before_out) as usize;
        input_pos += consumed;
        let new_len = result
            .len()
            .checked_add(produced)
            .context("zlib output size overflow")?;
        ensure!(
            new_len <= expected,
            "zlib output exceeds declared original size {original}"
        );
        result
            .try_reserve(produced)
            .context("allocate zlib output")?;
        result.extend_from_slice(&output[..produced]);
        if status == Status::StreamEnd {
            ensure!(
                remaining == 0 && input_pos == input_len && decoder.total_in() == stored,
                "trailing bytes after zlib stream"
            );
            ensure!(
                result.len() == expected,
                "zlib original size mismatch: expected {original}, got {}",
                result.len()
            );
            return Ok(result);
        }
        ensure!(
            consumed != 0 || produced != 0,
            "truncated or stalled zlib stream"
        );
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(size)
            .context("XP3 chunk position overflow")?;
        ensure!(
            end <= self.bytes.len(),
            "truncated XP3 chunk: need {size} bytes, have {}",
            self.bytes.len() - self.position
        );
        let bytes = &self.bytes[self.position..end];
        self.position = end;
        Ok(bytes)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn chunk(&mut self) -> Result<([u8; 4], &'a [u8])> {
        let tag = self.take(4)?.try_into()?;
        let size =
            usize::try_from(self.u64()?).context("XP3 chunk size cannot fit address space")?;
        Ok((tag, self.take(size)?))
    }
}

/// Adler-32 as specified by RFC 1950, over original concatenated file bytes.
pub fn adler32(bytes: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    // 5552 bounds intermediate sums to u32 even for all-0xff input.
    for block in bytes.chunks(5552) {
        for &byte in block {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// Deterministic non-security FNV-1a identity for indexes and save compatibility.
pub(crate) struct Fingerprint(u64);
impl Fingerprint {
    pub(crate) fn new() -> Self {
        Self(0xcbf29ce484222325)
    }
    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    }
    pub(crate) fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adler_known_vectors() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11e60398);
        let bytes = vec![255; 100_000];
        // Independently generated with Python's zlib.adler32.
        assert_eq!(adler32(&bytes), 0x149a302c);
    }
    #[test]
    fn path_normalization_is_strict() {
        assert_eq!(
            name_key("./UI\\Title//テスト.WEBP").unwrap(),
            "ui/title/テスト.webp"
        );
        for name in ["", ".", "../x", "a/../x", "/x", "\\x", "C:x", "x\0y"] {
            assert!(name_key(name).is_err(), "accepted {name:?}");
        }
    }
}
