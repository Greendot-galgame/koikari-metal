//! Audio playback: Vorbis music and effects stream through rodio, while Opus
//! voice clips (the bulk of the archive) are decoded to PCM buffers.
//!
//! Everything here degrades gracefully: if no output device exists the engine
//! reports itself unavailable and playback becomes a no-op, so the game still
//! runs on a headless machine. Optional voice lookup distinguishes missing media
//! from invalid names, corrupt data and decoder failures.

use crate::assets::AssetStore;
use anyhow::{Context, Result, anyhow, bail};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, buffer::SamplesBuffer};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

/// Output level per logical bus. BGM sits lower so voices stay intelligible.
#[derive(Clone, Copy, Debug)]
pub struct Volumes {
    pub bgm: f32,
    pub voice: f32,
    pub se: f32,
}

impl Default for Volumes {
    fn default() -> Self {
        Self {
            bgm: 0.65,
            voice: 1.0,
            se: 0.85,
        }
    }
}

/// Opus always decodes at 48 kHz; the archive's voices are 48 kHz too.
const OPUS_RATE: u32 = 48_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Vorbis,
    Opus,
}

/// Identify the Ogg payload by its first logical page.
fn classify(bytes: &[u8]) -> Result<Codec> {
    if bytes.len() < 27 {
        bail!("audio asset is shorter than an Ogg page header");
    }
    if &bytes[..4] != b"OggS" {
        bail!("audio asset is not an Ogg stream");
    }
    let segments = bytes[26] as usize;
    let payload = 27 + segments;
    if bytes.len() < payload {
        bail!("truncated Ogg segment table");
    }
    if bytes[payload..].starts_with(b"OpusHead") {
        return Ok(Codec::Opus);
    }
    if bytes[payload..].starts_with(b"\x01vorbis") {
        return Ok(Codec::Vorbis);
    }
    // KiriKiri ships no other audio container in this title.
    bail!("unsupported Ogg payload (neither Vorbis nor Opus)")
}

#[derive(Clone, Debug)]
struct Looping {
    storage: String,
    looping: bool,
}

/// Owns the output device plus one sink per bus/channel.
pub struct AudioEngine {
    stream: Option<OutputStream>,
    bgm: Option<Sink>,
    bgm_state: Option<Looping>,
    voice: Option<Sink>,
    se: BTreeMap<u32, Sink>,
    se_state: BTreeMap<u32, Looping>,
    volumes: Volumes,
    warning: Option<String>,
    /// Absent optional voice assets are logged once, never shown as persistent
    /// visual errors and never substituted with a different character's clip.
    missing_voices: BTreeSet<String>,
}

impl AudioEngine {
    /// Opens the default output device. A failure is recorded, not fatal.
    pub fn new() -> Self {
        match OutputStreamBuilder::open_default_stream() {
            Ok(stream) => Self {
                stream: Some(stream),
                bgm: None,
                bgm_state: None,
                voice: None,
                se: BTreeMap::new(),
                se_state: BTreeMap::new(),
                volumes: Volumes::default(),
                warning: None,
                missing_voices: BTreeSet::new(),
            },
            Err(error) => Self {
                stream: None,
                bgm: None,
                bgm_state: None,
                voice: None,
                se: BTreeMap::new(),
                se_state: BTreeMap::new(),
                volumes: Volumes::default(),
                warning: Some(format!(
                    "no audio output device ({error:#}); continuing silently"
                )),
                missing_voices: BTreeSet::new(),
            },
        }
    }

    pub fn available(&self) -> bool {
        self.stream.is_some()
    }

    pub fn volumes(&self) -> Volumes {
        self.volumes
    }

    pub fn set_volumes(&mut self, volumes: Volumes) {
        self.volumes = volumes;
        if let Some(sink) = &self.bgm {
            sink.set_volume(volumes.bgm);
        }
        if let Some(sink) = &self.voice {
            sink.set_volume(volumes.voice);
        }
        for sink in self.se.values() {
            sink.set_volume(volumes.se);
        }
    }

    /// Diagnostic text produced since the last call (normally empty).
    pub fn take_warning(&mut self) -> Option<String> {
        self.warning.take()
    }

    fn note(&mut self, message: String) {
        if self.warning.is_none() {
            self.warning = Some(message);
        }
    }

    fn new_sink(&self, volume: f32) -> Option<Sink> {
        let stream = self.stream.as_ref()?;
        let sink = Sink::connect_new(stream.mixer());
        sink.set_volume(volume);
        Some(sink)
    }

    /// Play background music, replacing whatever was playing.
    pub fn play_bgm(&mut self, assets: &AssetStore, storage: &str, looping: bool) -> Result<()> {
        // Validate required media even without an output device. Resolution only
        // consults the archive index; silent playback never reads its payload.
        assets
            .resolve(storage)
            .with_context(|| format!("resolve background music {storage:?}"))?;
        let Some(sink) = self.new_sink(self.volumes.bgm) else {
            return Ok(());
        };
        let bytes = assets
            .read(storage)
            .with_context(|| format!("load background music {storage:?}"))?;
        self.append_to(&sink, storage, &bytes)?;
        sink.play();
        if let Some(previous) = self.bgm.replace(sink) {
            previous.stop();
        }
        self.bgm_state = Some(Looping {
            storage: storage.to_string(),
            looping,
        });
        Ok(())
    }

    pub fn stop_bgm(&mut self) {
        if let Some(sink) = self.bgm.take() {
            sink.stop();
        }
        self.bgm_state = None;
    }

    /// Play one voice line, cancelling any previous line still speaking.
    /// Scripts in this edition refer to some voice numbers not present in either
    /// archive. True absence means an unvoiced line, not a game/rendering error.
    /// Bad aliases, I/O/checksum failures and decoder errors are still reported.
    pub fn play_voice(&mut self, assets: &AssetStore, storage: &str) -> Result<()> {
        self.stop_voice();
        let Some(bytes) = assets
            .read_optional(storage)
            .with_context(|| format!("load voice {storage:?}"))?
        else {
            if self.missing_voices.insert(storage.to_owned()) {
                eprintln!(
                    "audio: voice {storage:?} is not included in the local archives; continuing this line silently"
                );
            }
            return Ok(());
        };
        let Some(sink) = self.new_sink(self.volumes.voice) else {
            return Ok(());
        };
        self.append_to(&sink, storage, &bytes)?;
        sink.play();
        if let Some(previous) = self.voice.replace(sink) {
            previous.stop();
        }
        Ok(())
    }

    pub fn stop_voice(&mut self) {
        if let Some(sink) = self.voice.take() {
            sink.stop();
        }
    }

    /// Play a sound effect on a KAG channel, replacing that channel's sound.
    pub fn play_se(
        &mut self,
        assets: &AssetStore,
        storage: &str,
        looping: bool,
        channel: u32,
    ) -> Result<()> {
        assets
            .resolve(storage)
            .with_context(|| format!("resolve sound effect {storage:?}"))?;
        let Some(sink) = self.new_sink(self.volumes.se) else {
            return Ok(());
        };
        let bytes = assets
            .read(storage)
            .with_context(|| format!("load sound effect {storage:?}"))?;
        self.append_to(&sink, storage, &bytes)?;
        sink.play();
        if let Some(previous) = self.se.insert(channel, sink) {
            previous.stop();
        }
        self.se_state.insert(
            channel,
            Looping {
                storage: storage.to_string(),
                looping,
            },
        );
        Ok(())
    }

    pub fn stop_se(&mut self, channel: u32) {
        if let Some(sink) = self.se.remove(&channel) {
            sink.stop();
        }
        self.se_state.remove(&channel);
    }

    pub fn stop_all(&mut self) {
        self.stop_bgm();
        self.stop_voice();
        for (_, sink) in std::mem::take(&mut self.se) {
            sink.stop();
        }
        self.se_state.clear();
    }

    /// Append either a streaming Vorbis decoder or a decoded Opus buffer.
    /// An error must leave a new sink empty, never register a pretend playback.
    fn append_to(&mut self, sink: &Sink, storage: &str, bytes: &[u8]) -> Result<()> {
        match classify(bytes).with_context(|| format!("inspect audio {storage:?}"))? {
            Codec::Vorbis => {
                let new_decoder = || {
                    Decoder::new(Cursor::new(bytes.to_vec()))
                        .map_err(|error| anyhow!("decode Vorbis {storage:?}: {error}"))
                };
                // An initial zero-length span can precede valid Vorbis data.
                // Probe one sample instead, then discard this decoder and build
                // a fresh streaming source so playback retains its first sample.
                let mut probe = new_decoder()?;
                if probe.next().is_none() {
                    bail!("audio asset {storage:?} decoded to no samples");
                }
                drop(probe);
                sink.append(new_decoder()?);
                Ok(())
            }
            Codec::Opus => {
                let (channels, rate, samples) =
                    decode_opus(bytes).with_context(|| format!("decode Opus {storage:?}"))?;
                if samples.is_empty() {
                    bail!("audio asset {storage:?} decoded to no samples");
                }
                sink.append(SamplesBuffer::new(channels, rate, samples));
                Ok(())
            }
        }
    }

    /// Restart looping music/effects that have reached their end.
    ///
    /// Looping is done by re-queuing the source rather than by cloning it, so a
    /// nine-megabyte Vorbis track is never expanded into RAM. Failed restarts are
    /// stopped, not retried every frame; a later explicit play can try again.
    pub fn poll(&mut self, assets: &AssetStore) {
        if let Some(state) = self.bgm_state.clone() {
            let finished = self.bgm.as_ref().is_some_and(Sink::empty);
            if state.looping && finished {
                match self.play_bgm(assets, &state.storage, true) {
                    Err(error) => {
                        self.stop_bgm();
                        self.note(format!("restart background music: {error:#}"));
                    }
                    // Silent playback is Ok but cannot replace the old empty
                    // sink. Drop its loop state instead of retrying forever.
                    Ok(()) if !self.available() => self.stop_bgm(),
                    Ok(()) => {}
                }
            }
        }
        for (channel, state) in self.se_state.clone() {
            let finished = self.se.get(&channel).is_some_and(Sink::empty);
            if state.looping && finished {
                match self.play_se(assets, &state.storage, true, channel) {
                    Err(error) => {
                        self.stop_se(channel);
                        self.note(format!("restart sound effect: {error:#}"));
                    }
                    Ok(()) if !self.available() => self.stop_se(channel),
                    Ok(()) => {}
                }
            }
        }
    }
}

/// Decode an Ogg/Opus stream into interleaved f32 samples.
fn decode_opus(bytes: &[u8]) -> Result<(u16, u32, Vec<f32>)> {
    let mut reader = ogg::PacketReader::new(Cursor::new(bytes));
    let head = reader
        .read_packet()?
        .context("Opus stream has no identification header")?;
    let data = head.data.as_slice();
    if !data.starts_with(b"OpusHead") || data.len() < 19 {
        bail!("malformed OpusHead packet");
    }
    let channels = data[9];
    if !(1..=2).contains(&channels) {
        bail!("unsupported Opus channel count {channels}");
    }
    let pre_skip = u16::from_le_bytes([data[10], data[11]]) as usize;
    let opus_channels = if channels == 1 {
        opus::Channels::Mono
    } else {
        opus::Channels::Stereo
    };
    let mut decoder = opus::Decoder::new(OPUS_RATE, opus_channels)
        .map_err(|error| anyhow!("create Opus decoder: {error}"))?;
    let mut samples: Vec<f32> = Vec::new();
    while let Some(packet) = reader.read_packet()? {
        let data = packet.data;
        if data.starts_with(b"OpusTags") {
            continue;
        }
        if data.is_empty() {
            continue;
        }
        let per_channel = decoder
            .get_nb_samples(&data)
            .map_err(|error| anyhow!("inspect Opus packet: {error}"))?;
        if per_channel == 0 {
            continue;
        }
        let start = samples.len();
        samples.resize(start + per_channel * channels as usize, 0.0);
        let decoded = decoder
            .decode_float(&data, &mut samples[start..], false)
            .map_err(|error| anyhow!("decode Opus packet: {error}"))?;
        samples.truncate(start + decoded * channels as usize);
    }
    // Drop the decoder priming samples so speech starts immediately.
    let skip = pre_skip * channels as usize;
    if skip > 0 && samples.len() > skip {
        samples.drain(..skip);
    }
    Ok((u16::from(channels), OPUS_RATE, samples))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn silent_engine() -> AudioEngine {
        AudioEngine {
            stream: None,
            bgm: None,
            bgm_state: None,
            voice: None,
            se: BTreeMap::new(),
            se_state: BTreeMap::new(),
            volumes: Volumes::default(),
            warning: None,
            missing_voices: BTreeSet::new(),
        }
    }

    /// One empty, synthetic index entry; no original archives or media are used.
    struct SyntheticAssets {
        directory: PathBuf,
        assets: AssetStore,
    }

    impl SyntheticAssets {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "koikari-audio-test-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
                let mut bytes = tag.to_vec();
                bytes.extend_from_slice(&(body.len() as u64).to_le_bytes());
                bytes.extend_from_slice(body);
                bytes
            }
            let name: Vec<u16> = "stub.ogg".encode_utf16().collect();
            let mut info = vec![0; 20]; // flags, original size, stored size
            info.extend_from_slice(&(name.len() as u16).to_le_bytes());
            for unit in name {
                info.extend_from_slice(&unit.to_le_bytes());
            }
            let mut body = chunk(b"info", &info);
            body.extend(chunk(b"segm", &[]));
            body.extend(chunk(b"adlr", &1u32.to_le_bytes()));
            let index = chunk(b"File", &body);
            let mut archive = crate::archive::XP3_MAGIC.to_vec();
            archive.extend_from_slice(&19u64.to_le_bytes());
            archive.push(0); // uncompressed, final index block
            archive.extend_from_slice(&(index.len() as u64).to_le_bytes());
            archive.extend(index);
            std::fs::write(directory.join("data.xp3"), archive).unwrap();
            let assets = AssetStore::open_base(&directory).unwrap();
            Self { directory, assets }
        }
    }

    impl Drop for SyntheticAssets {
        fn drop(&mut self) {
            // Only remove the exact file and exclusive directory this fixture
            // created, after verifying their canonical absolute paths.
            let directory = self.directory.canonicalize().unwrap();
            assert_eq!(directory, self.directory);
            assert_eq!(
                directory.parent(),
                Some(std::env::temp_dir().canonicalize().unwrap().as_path())
            );
            assert!(
                directory
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("koikari-audio-test-")
            );
            let archive = directory.join("data.xp3");
            assert_eq!(archive.canonicalize().unwrap(), archive);
            std::fs::remove_file(archive).unwrap();
            std::fs::remove_dir(directory).unwrap();
        }
    }

    fn synthetic_ogg_packet(packet: Vec<u8>) -> Vec<u8> {
        let mut writer = ogg::PacketWriter::new(Vec::new());
        writer
            .write_packet(packet, 1, ogg::PacketWriteEndInfo::EndStream, 0)
            .unwrap();
        writer.into_inner()
    }

    #[test]
    fn synthetic_bad_audio_is_an_error_and_never_queues_a_source() {
        let mut engine = silent_engine();
        let (sink, _unconnected_queue) = Sink::new();
        for bytes in [
            b"bad".to_vec(),
            vec![0; 32],
            synthetic_ogg_packet(b"unsupported codec".to_vec()),
            synthetic_ogg_packet(b"\x01vorbis invalid header".to_vec()),
            synthetic_ogg_packet(b"OpusHead".to_vec()),
        ] {
            let error = engine.append_to(&sink, "bad.ogg", &bytes).unwrap_err();
            assert!(format!("{error:#}").contains("bad.ogg"));
            assert!(sink.empty());
            assert!(engine.bgm_state.is_none() && engine.se_state.is_empty());
            assert!(engine.take_warning().is_none(), "caller receives the error");
        }
    }

    #[test]
    fn synthetic_header_only_opus_cannot_register_empty_playback() {
        let mut engine = silent_engine();
        let (sink, _unconnected_queue) = Sink::new();
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 1]); // version 1, mono
        head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
        head.extend_from_slice(&OPUS_RATE.to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes()); // output gain
        head.push(0); // mapping family
        let bytes = synthetic_ogg_packet(head);
        assert!(decode_opus(&bytes).unwrap().2.is_empty());
        let error = engine.append_to(&sink, "empty.opus", &bytes).unwrap_err();
        assert!(format!("{error:#}").contains("decoded to no samples"));
        assert!(sink.empty());
    }

    #[test]
    fn synthetic_failed_bgm_restart_drops_loop_and_warns_only_once() {
        let fixture = SyntheticAssets::new();
        let mut engine = silent_engine();
        let (sink, _unconnected_queue) = Sink::new();
        engine.bgm = Some(sink);
        engine.bgm_state = Some(Looping {
            storage: "../invalid".into(),
            looping: true,
        });
        engine.poll(&fixture.assets);
        assert!(engine.bgm.is_none() && engine.bgm_state.is_none());
        let warning = engine.take_warning().unwrap();
        assert!(warning.contains("restart background music"));
        assert!(warning.contains("path traversal"));
        for _ in 0..64 {
            engine.poll(&fixture.assets);
            assert!(engine.take_warning().is_none());
            assert!(engine.bgm.is_none() && engine.bgm_state.is_none());
        }
    }

    #[test]
    fn synthetic_failed_se_restart_only_clears_its_channel() {
        let fixture = SyntheticAssets::new();
        let mut engine = silent_engine();
        let (bgm, _bgm_queue) = Sink::new();
        bgm.append(SamplesBuffer::new(1, OPUS_RATE, vec![0.0]));
        engine.bgm = Some(bgm);
        engine.bgm_state = Some(Looping {
            storage: "../must-not-restart-unfinished-bgm".into(),
            looping: true,
        });
        let (failed, _failed_queue) = Sink::new();
        engine.se.insert(7, failed);
        engine.se_state.insert(
            7,
            Looping {
                storage: "../invalid".into(),
                looping: true,
            },
        );
        let (active, _active_queue) = Sink::new();
        active.append(SamplesBuffer::new(1, OPUS_RATE, vec![0.0]));
        engine.se.insert(8, active);
        engine.se_state.insert(
            8,
            Looping {
                storage: "../must-not-restart-unfinished".into(),
                looping: true,
            },
        );
        let (once, _once_queue) = Sink::new();
        engine.se.insert(9, once);
        engine.se_state.insert(
            9,
            Looping {
                storage: "../must-not-restart-one-shot".into(),
                looping: false,
            },
        );
        engine.poll(&fixture.assets);
        assert!(!engine.se.contains_key(&7) && !engine.se_state.contains_key(&7));
        let warning = engine.take_warning().unwrap();
        assert!(warning.contains("restart sound effect"));
        assert!(warning.contains("path traversal"));
        for _ in 0..64 {
            engine.poll(&fixture.assets);
            assert!(engine.take_warning().is_none());
            assert!(engine.bgm.as_ref().is_some_and(|sink| !sink.empty()));
            assert!(engine.bgm_state.as_ref().is_some_and(|state| state.looping));
            for channel in [8, 9] {
                assert!(engine.se.contains_key(&channel));
                assert!(engine.se_state.contains_key(&channel));
            }
        }
    }

    #[test]
    fn synthetic_no_device_restart_success_does_not_keep_empty_loops() {
        let fixture = SyntheticAssets::new();
        let mut engine = silent_engine();
        let (bgm, _bgm_queue) = Sink::new();
        engine.bgm = Some(bgm);
        engine.bgm_state = Some(Looping {
            storage: "stub.ogg".into(),
            looping: true,
        });
        let (se, _se_queue) = Sink::new();
        engine.se.insert(7, se);
        engine.se_state.insert(
            7,
            Looping {
                storage: "stub.ogg".into(),
                looping: true,
            },
        );
        for _ in 0..64 {
            engine.poll(&fixture.assets);
            assert!(engine.bgm.is_none() && engine.bgm_state.is_none());
            assert!(engine.se.is_empty() && engine.se_state.is_empty());
            assert!(engine.take_warning().is_none());
        }
    }

    #[test]
    fn synthetic_missing_optional_voice_stays_silent_and_deduplicated() {
        let fixture = SyntheticAssets::new();
        let mut engine = silent_engine();
        for _ in 0..3 {
            engine.play_voice(&fixture.assets, "missing_voice").unwrap();
            assert!(engine.voice.is_none());
            assert!(engine.take_warning().is_none());
        }
        assert_eq!(engine.missing_voices.len(), 1);
        assert!(engine.play_voice(&fixture.assets, "../invalid").is_err());
    }

    #[test]
    fn classifies_ogg_payloads() {
        let mut page = vec![0u8; 27];
        page[..4].copy_from_slice(b"OggS");
        page[26] = 1; // one segment
        let mut opus = page.clone();
        opus.push(0);
        opus.extend_from_slice(b"OpusHead");
        assert_eq!(classify(&opus).unwrap(), Codec::Opus);

        let mut vorbis = page;
        vorbis.push(0);
        vorbis.extend_from_slice(b"\x01vorbis");
        assert_eq!(classify(&vorbis).unwrap(), Codec::Vorbis);
    }

    #[test]
    fn owned_ogg_assets_initialize_and_queue_without_playback() {
        let Some(root) = std::env::var_os("KOIKARI_GAME_ROOT") else {
            eprintln!("real Vorbis initialization regression skipped: set KOIKARI_GAME_ROOT");
            return;
        };
        // Use the normal data/additional/patch namespace. Asset reads verify
        // checksums, but only Vorbis's initial span is decoded; sinks and their
        // queues stay disconnected from any mixer or output device.
        let assets = AssetStore::open(root).unwrap();
        let mut engine = silent_engine();
        let mut validated = 0;
        for storage in assets
            .names()
            .into_iter()
            .filter(|name| name.to_ascii_lowercase().ends_with(".ogg"))
        {
            let bytes = assets
                .read(&storage)
                .unwrap_or_else(|error| panic!("read/checksum {storage:?}: {error:#}"));
            // Do not accidentally fully decode an Opus payload in this
            // initialization-only regression for archived music and effects.
            assert_eq!(
                classify(&bytes).unwrap_or_else(|error| panic!("inspect {storage:?}: {error:#}")),
                Codec::Vorbis,
                "expected streaming Vorbis music/effect {storage:?}"
            );
            let (sink, _unconnected_queue) = Sink::new();
            engine
                .append_to(&sink, &storage, &bytes)
                .unwrap_or_else(|error| panic!("initialize {storage:?}: {error:#}"));
            assert!(
                !sink.empty(),
                "valid Vorbis source was not queued: {storage:?}"
            );
            assert!(engine.take_warning().is_none());
            validated += 1;
        }
        assert!(
            validated > 0,
            "no Ogg music/effects found in supplied archives"
        );
        eprintln!("validated {validated} Vorbis music/effect assets without playback");
    }

    fn owned_assets() -> Option<AssetStore> {
        let Some(root) = std::env::var_os("KOIKARI_GAME_ROOT") else {
            eprintln!("real voice regression skipped: set KOIKARI_GAME_ROOT");
            return None;
        };
        // Keep the original missing-voice regression independent of optional
        // packs that may now supply those previously absent recordings.
        Some(AssetStore::open_base(root).unwrap())
    }

    #[test]
    fn reported_missing_voices_continue_silently_without_ui_warnings_or_replacement_audio() {
        let Some(assets) = owned_assets() else {
            return;
        };
        // No output device is opened, so this regression cannot play sound.
        let mut engine = AudioEngine {
            stream: None,
            bgm: None,
            bgm_state: None,
            voice: None,
            se: BTreeMap::new(),
            se_state: BTreeMap::new(),
            volumes: Volumes::default(),
            warning: None,
            missing_voices: BTreeSet::new(),
        };
        for _ in 0..3 {
            for storage in ["vo5_1089", "vo5_1169"] {
                assert_eq!(assets.resolve_optional(storage).unwrap(), None);
                engine.play_voice(&assets, storage).unwrap();
                assert!(engine.take_warning().is_none());
                assert!(engine.voice.is_none());
            }
        }
        assert_eq!(
            engine.missing_voices.len(),
            2,
            "missing clips should be diagnosed only once per engine"
        );
        assert!(
            engine.play_voice(&assets, "../invalid").is_err(),
            "invalid media names must not become silent absence"
        );
    }

    #[test]
    fn existing_neighbours_of_missing_voices_still_decode_to_real_pcm() {
        let Some(assets) = owned_assets() else {
            return;
        };
        for storage in ["vo5_1088", "vo5_1090", "vo5_1168", "vo5_1170"] {
            let bytes = assets
                .read_optional(storage)
                .unwrap()
                .expect("neighbouring voice should be present");
            assert_eq!(classify(&bytes).unwrap(), Codec::Opus);
            let (channels, rate, pcm) = decode_opus(&bytes).unwrap();
            assert!((1..=2).contains(&channels));
            assert_eq!(rate, OPUS_RATE);
            assert!(!pcm.is_empty());
            assert!(pcm.iter().all(|sample| sample.is_finite()));
        }
    }

    #[test]
    fn additional_archive_voice_payloads_decode_without_playing_or_substitution() {
        let Some(root) = std::env::var_os("KOIKARI_GAME_ROOT") else {
            return;
        };
        if !std::path::Path::new(&root).join("adult.xp3").is_file() {
            return;
        }
        let store = AssetStore::open(root).unwrap();
        assert!(store.has_archive("adult.xp3"));
        for storage in ["vo5_1089", "vo5_1169"] {
            let bytes = store
                .read_optional(storage)
                .unwrap()
                .expect("additional archive supplies the original clip");
            assert_eq!(classify(&bytes).unwrap(), Codec::Opus);
            let (channels, rate, pcm) = decode_opus(&bytes).unwrap();
            assert!((1..=2).contains(&channels));
            assert_eq!(rate, OPUS_RATE);
            assert!(!pcm.is_empty() && pcm.iter().all(|sample| sample.is_finite()));
        }
    }

    #[test]
    fn rejects_non_ogg_bytes() {
        assert!(classify(b"RIFF0000WAVE").is_err());
        assert!(classify(b"OggS").is_err());
    }
}
