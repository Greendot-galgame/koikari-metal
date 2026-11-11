//! Read-only format/overlay tests for an optional local XP3 resource pack.
//! No images or scene text are displayed and no original TJS/plugin is executed.
use koikari_metal::{
    assets::AssetStore,
    runtime::{Event, FIRST_SCENARIO, Runtime},
    script::{InstructionKind, Program},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

fn root() -> Option<PathBuf> {
    let root = std::env::var_os("KOIKARI_ASSET_ROOT")
        .or_else(|| std::env::var_os("KOIKARI_GAME_ROOT"))
        .map(PathBuf::from)?;
    if !root.join("adult.xp3").exists() {
        eprintln!("additional XP3 test skipped: no optional pack");
        return None;
    }
    Some(root)
}

fn identity(path: &Path) -> (u64, std::time::SystemTime) {
    let metadata = std::fs::metadata(path).unwrap();
    (metadata.len(), metadata.modified().unwrap())
}

#[test]
fn real_additional_archive_joins_the_namespace_and_keeps_patch_priority_read_only() {
    let Some(root) = root() else {
        return;
    };
    let paths = ["data.xp3", "adult.xp3", "patch.xp3"].map(|name| root.join(name));
    let before = paths.each_ref().map(|path| identity(path));
    let store = AssetStore::open(&root).unwrap();
    assert_eq!(
        store
            .archives()
            .iter()
            .map(|a| a.path().file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        ["data.xp3", "adult.xp3", "patch.xp3"]
    );
    let additional = &store.archives()[1];
    assert_eq!(additional.stats().archive_bytes, 2_040_430_649);
    assert_eq!(additional.stats().entry_count, 4554);
    let unique: BTreeSet<_> = store
        .archives()
        .iter()
        .flat_map(|archive| archive.entries().keys())
        .collect();
    assert_eq!(store.stats().entry_count, unique.len());
    assert_eq!(store.stats().archive_count, 3);
    let base = AssetStore::open_base(&root).unwrap();
    assert_ne!(base.fingerprint(), store.fingerprint());
    for voice in ["vo5_1089", "vo5_1169"] {
        assert!(base.resolve_optional(voice).unwrap().is_none());
        let canonical = format!("voice/vo5/{voice}.opus");
        let bytes = store.read(voice).unwrap();
        assert_eq!(bytes, additional.read(&canonical).unwrap());
        assert!(bytes.starts_with(b"OggS"));
    }
    let patch = &store.archives()[2];
    let patched = patch.read("ev703_1.webp").unwrap();
    for alias in [
        "ev703_1",
        "evimage/ev703_1.webp",
        "data/evimage/ev703_1.webp",
    ] {
        assert_eq!(store.read(alias).unwrap(), patched);
    }
    assert_eq!(before, paths.each_ref().map(|path| identity(path)));
    eprintln!(
        "additional archive: {} members; {} combined exact-path entries",
        additional.stats().entry_count,
        store.stats().entry_count
    );
}

#[test]
fn additional_metadata_loads_and_normal_opening_still_runs_without_executing_tjs() {
    let Some(root) = root() else {
        return;
    };
    let mut base = Runtime::new(Arc::new(AssetStore::open_base(&root).unwrap())).unwrap();
    assert_eq!(base.start().unwrap(), Event::Dialogue);
    let old = base.snapshot();
    let mut combined = Runtime::new(Arc::new(AssetStore::open(&root).unwrap())).unwrap();
    assert!(combined.chapters().len() > base.chapters().len());
    assert_eq!(combined.start().unwrap(), Event::Dialogue);
    assert_eq!(combined.state.scenario, FIRST_SCENARIO);
    assert_eq!(combined.state.background.as_deref(), Some("bg1_0"));
    assert_eq!(combined.state.sprites.len(), 1);
    assert!(combined.state.diagnostic.is_none());
    // Script identities changed. Refuse to transplant a legacy program counter;
    // base-only mode and separate default save paths preserve the old profile.
    assert!(combined.restore(old).is_err());
    eprintln!(
        "metadata: {} base chapters, {} combined chapters; ordinary opening verified",
        base.chapters().len(),
        combined.chapters().len()
    );
}

#[test]
fn all_additional_scene_initializers_reach_a_dialogue_without_media_output() {
    let Some(root) = root() else {
        return;
    };
    let store = Arc::new(AssetStore::open(root).unwrap());
    let mut runtime = Runtime::new(store.clone()).unwrap();
    let chapters: Vec<_> = runtime
        .chapters()
        .iter()
        .map(|c| c.storage.clone())
        .collect();
    let mut checked = 0;
    for storage in chapters {
        let program = Program::compile(&storage, &store.text(&storage).unwrap()).unwrap();
        let has_scene_start = program.instructions.iter().any(|instruction| {
            matches!(&instruction.kind,
            InstructionKind::Command(tag) if tag.name.eq_ignore_ascii_case("SC_SceneStartProcess"))
        });
        if !has_scene_start {
            continue;
        }
        let event = runtime.start_at(&storage, None).unwrap();
        assert_eq!(
            event,
            Event::Dialogue,
            "initializer blocked at {storage}:{}: {:?}",
            runtime.state.source_line,
            runtime.state.diagnostic
        );
        assert!(runtime.state.diagnostic.is_none());
        assert_eq!(runtime.variables["f.HSceneCheck"], serde_json::json!(true));
        runtime.take_effects();
        checked += 1;
    }
    assert_eq!(checked, 26);
    eprintln!(
        "scene lifecycle: {checked} additional initializers validated without image/audio output"
    );
}

#[test]
fn reported_chapter_passes_scene_start_and_whiteout_and_reaches_its_next_chapter() {
    let Some(root) = root() else {
        return;
    };
    let store = Arc::new(AssetStore::open(root).unwrap());
    let storage = "scenario/main/5_3.ks";
    let program = Program::compile(storage, &store.text(storage).unwrap()).unwrap();
    let whiteout_line = program
        .instructions
        .iter()
        .find_map(|instruction| match &instruction.kind {
            InstructionKind::Command(tag) if tag.name.eq_ignore_ascii_case("whiteout") => {
                Some(instruction.line)
            }
            _ => None,
        })
        .expect("regression chapter must contain the white-flash instruction");
    let mut runtime = Runtime::new(store).unwrap();
    let mut event = runtime.start_at(storage, None).unwrap();
    let mut passed_whiteout = false;
    let mut stops = 0;
    for n in 0..4000 {
        assert_ne!(
            event,
            Event::Blocked,
            "blocked at {}:{}: {:?}",
            runtime.state.scenario,
            runtime.state.source_line,
            runtime.state.diagnostic
        );
        if runtime.state.scenario != storage {
            break;
        }
        assert_eq!(
            event,
            Event::Dialogue,
            "unexpected control-flow stop at line {}",
            runtime.state.source_line
        );
        passed_whiteout |= runtime.state.source_line >= whiteout_line;
        assert!(n < 3999, "regression chapter did not terminate");
        runtime.take_effects();
        event = runtime.advance().unwrap();
        stops += 1;
    }
    runtime.take_effects();
    assert!(passed_whiteout && stops > 2);
    assert_eq!(runtime.state.scenario, "scenario/main/5_4.ks");
    assert_eq!(runtime.variables["f.HSceneCheck"], serde_json::json!(false));
    assert!(runtime.state.diagnostic.is_none());
    eprintln!(
        "scene lifecycle: reported chapter passed {stops} dialogue stops and its final transition without media output"
    );
}

#[test]
fn the_reported_legacy_error_save_retries_the_real_instruction_at_line_eight() {
    let Some(root) = root() else {
        return;
    };
    let store = Arc::new(AssetStore::open(root).unwrap());
    let storage = "scenario/main/5_3.ks";
    let program = Program::compile(storage, &store.text(storage).unwrap()).unwrap();
    let index = program
        .instructions
        .iter()
        .position(|instruction| {
            matches!(&instruction.kind,
        InstructionKind::Command(tag) if tag.name.eq_ignore_ascii_case("SC_SceneStartProcess"))
        })
        .unwrap();
    assert_eq!(program.instructions[index].line, 8);
    let mut runtime = Runtime::new(store).unwrap();
    assert_eq!(runtime.start_at(storage, None).unwrap(), Event::Dialogue);
    runtime.take_effects();
    let mut old = runtime.snapshot();
    old.location.pc = index;
    old.state.text.clear();
    old.state.speaker.clear();
    old.state.voice = None;
    old.state.source_line = 8;
    old.state.waiting = true;
    old.state.messages_seen = 0;
    old.history.clear();
    old.variables.remove("f.HSceneCheck");
    old.variables.remove("tf.SCENE_Enabled");
    old.state.diagnostic = Some(format!(
        "{storage}:8: unknown command [SC_SceneStartProcess]; no implicit no-op or guessed branch"
    ));
    runtime.restore(old).unwrap();
    runtime.take_effects();
    assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
    assert!(runtime.location().pc > index);
    assert!(runtime.state.diagnostic.is_none());
    assert_eq!(runtime.variables["f.HSceneCheck"], serde_json::json!(true));
}
