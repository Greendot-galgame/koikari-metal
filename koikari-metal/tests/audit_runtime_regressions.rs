//! Synthetic regressions discovered by the comprehensive audit.
use koikari_metal::{
    runtime::{AudioEffect, Event, Runtime},
    save,
};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

fn vm(source: &str) -> Runtime {
    Runtime::from_scripts(
        BTreeMap::from([("main.ks".into(), source.into())]),
        "main.ks",
    )
    .unwrap()
}

#[test]
fn loading_and_new_game_stop_one_shot_audio_and_discard_old_pending_effects() {
    let mut runtime = vm(
        "[地]saved[c][playse storage=long buf=7 loop=false][playse storage=rain buf=8 loop=true][地]later[c]",
    );
    runtime.start().unwrap();
    let snapshot = runtime.snapshot();
    runtime.take_effects();
    runtime.advance().unwrap();
    // Do not drain: these pending old-world sounds must never play after load.
    runtime.restore(snapshot).unwrap();
    let effects = runtime.take_effects();
    assert_eq!(effects.first(), Some(&AudioEffect::StopAll));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, AudioEffect::Se { channel: 7 | 8, .. }))
    );
    runtime.advance().unwrap();
    runtime.new_game().unwrap();
    let effects = runtime.take_effects();
    assert_eq!(effects.first(), Some(&AudioEffect::StopAll));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, AudioEffect::Se { channel: 7 | 8, .. }))
    );
}

#[test]
fn inspection_restart_resets_warning_dedup_and_save_future_is_identical() {
    let source = "[wait time=10][地]one[c][wait time=10][地]two[c]";
    let mut runtime = vm(source);
    runtime.start_at("main.ks", None).unwrap();
    runtime.start_at("main.ks", None).unwrap();
    assert_eq!(runtime.state.warnings.len(), 1);
    let mut resumed = vm(source);
    resumed.restore(runtime.snapshot()).unwrap();
    assert_eq!(runtime.advance().unwrap(), resumed.advance().unwrap());
    assert!(runtime.state == resumed.state);
}

#[test]
fn restarting_an_execution_does_not_save_unrelated_cached_scenario_identities() {
    let mut runtime = Runtime::from_scripts(
        BTreeMap::from([
            (
                "main.ks".into(),
                "[地]one[c][call storage=later.ks][地]three[c]".into(),
            ),
            ("later.ks".into(), "[地]two[c][return]".into()),
        ]),
        "main.ks",
    )
    .unwrap();
    runtime.start().unwrap();
    runtime.advance().unwrap();
    assert!(
        runtime
            .snapshot()
            .source_fingerprints
            .contains_key("later.ks")
    );
    runtime.advance().unwrap();
    assert_eq!(runtime.state.scenario, "main.ks");
    runtime.new_game().unwrap();
    assert_eq!(runtime.snapshot().source_fingerprints.len(), 1);
    assert!(
        runtime
            .snapshot()
            .source_fingerprints
            .contains_key("main.ks")
    );
}

#[test]
fn invalid_transform_cannot_replace_a_valid_save_and_invalid_alpha_is_rejected() {
    let mut runtime = vm("[stc ch=1 f=111][地]one[c]");
    assert_eq!(runtime.start().unwrap(), Event::Dialogue);
    let valid = runtime.snapshot();
    let root = std::env::temp_dir().canonicalize().unwrap();
    let directory = root.join(format!(
        "koikari-save-audit-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let expected = directory.canonicalize().unwrap();
    assert_eq!(expected.parent(), Some(root.as_path()));
    let file = directory.join("quicksave.json");
    save::write_snapshot(&file, &valid).unwrap();
    let original = std::fs::read(&file).unwrap();
    let mut invalid = valid.clone();
    invalid.state.sprites[0].x = f32::NAN;
    assert!(save::write_snapshot(&file, &invalid).is_err());
    assert!(std::fs::read(&file).unwrap() == original);
    for opacity in [-1.0, 1.1, f32::INFINITY] {
        let mut invalid = valid.clone();
        invalid.state.sprites[0].opacity = opacity;
        assert!(save::to_json(&invalid).is_err());
        assert!(runtime.restore(invalid).is_err());
        assert!(runtime.snapshot() == valid);
    }
    assert_eq!(directory.canonicalize().unwrap(), expected);
    assert_eq!(
        file.canonicalize().unwrap(),
        expected.join("quicksave.json")
    );
    assert!(
        expected
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("koikari-save-audit-")
    );
    std::fs::remove_file(&file).unwrap();
    std::fs::remove_dir(&expected).unwrap();
}
