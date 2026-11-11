//! Synthetic, non-visual lifecycle regressions. No game dialogue/media is used.
use koikari_metal::{
    runtime::{AudioEffect, Event, Runtime},
    save,
};
use serde_json::json;
use std::collections::BTreeMap;

fn vm(source: &str) -> Runtime {
    Runtime::from_scripts(
        BTreeMap::from([("main.ks".into(), source.into())]),
        "main.ks",
    )
    .unwrap()
}

#[test]
fn argumentless_scene_start_preserves_inherited_scene_and_end_only_clears_the_flag() {
    let source = "[SC_StartProcess bg=room bgm=theme ms=2][stc ch=1 f=111 pc][ms_hide standhide][SC_SceneStartProcess bgm=ignored ms_show=false][地]one[c]\n[SC_SceneEnd][地]two[c]";
    let mut runtime = vm(source);
    assert_eq!(runtime.start().unwrap(), Event::Dialogue);
    assert_eq!(runtime.state.background.as_deref(), Some("room"));
    assert_eq!(runtime.state.bgm.as_deref(), Some("theme"));
    assert_eq!(runtime.state.message_style, 2);
    assert!(!runtime.state.message_visible);
    assert_eq!(runtime.state.sprites.len(), 1);
    assert_eq!(runtime.variables["f.HSceneCheck"], json!(true));
    assert_eq!(runtime.variables["f.ADV_SerifLineCount"], json!(1));
    let original_sprites = runtime.state.sprites.clone();
    let snapshot = save::from_json(&save::to_json(&runtime.snapshot()).unwrap()).unwrap();
    let mut restored = vm(source);
    restored.restore(snapshot).unwrap();
    assert_eq!(restored.variables["f.HSceneCheck"], json!(true));
    restored.take_effects();
    assert_eq!(restored.advance().unwrap(), Event::Dialogue);
    assert_eq!(restored.variables["f.HSceneCheck"], json!(false));
    assert_eq!(restored.variables["f.ADV_SerifLineCount"], json!(2));
    assert_eq!(restored.state.background.as_deref(), Some("room"));
    assert_eq!(restored.state.bgm.as_deref(), Some("theme"));
    assert_eq!(restored.state.sprites, original_sprites);
    assert!(!restored.state.message_visible);
    assert!(
        !restored
            .take_effects()
            .iter()
            .any(|e| matches!(e, AudioEffect::StopBgm))
    );
}

#[test]
fn explicit_background_uses_bgset_defaults_and_missing_bgm_does_not_stop_music() {
    let mut runtime = vm(
        "[SC_StartProcess bg=old bgm=theme ms=2][stc ch=1 f=111][地]one[c]\n[ms_hide standhide][SC_SceneStartProcess bg=new ms=9 ms_show=false][地]two[c]\n[SC_SceneStartProcess bg=last bgm=stop][地]three[c]",
    );
    runtime.start().unwrap();
    runtime.take_effects();
    assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
    assert_eq!(runtime.state.background.as_deref(), Some("new"));
    assert_eq!(runtime.state.bgm.as_deref(), Some("theme"));
    assert!(runtime.state.sprites.is_empty());
    assert_eq!(runtime.state.message_style, 0);
    assert!(runtime.state.message_visible);
    assert_eq!(runtime.variables["f.MS_BaseImageType"], json!(0));
    assert!(
        !runtime
            .take_effects()
            .iter()
            .any(|e| matches!(e, AudioEffect::StopBgm))
    );
    assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
    assert_eq!(runtime.state.background.as_deref(), Some("last"));
    assert!(runtime.state.bgm.is_none());
    assert!(
        runtime
            .take_effects()
            .iter()
            .any(|e| matches!(e, AudioEffect::StopBgm))
    );
}

#[test]
fn chapter_end_invokes_scene_cleanup_before_following_its_authored_destination() {
    let mut runtime = Runtime::from_scripts(BTreeMap::from([
        ("scenario/main/main.ks".into(), "[SC_StartProcess bg=old bgm=theme][SC_SceneStartProcess][地]one[c]\n[SC_EndProcess storage=sub]".into()),
        ("scenario/main/sub.ks".into(), "[SC_StartProcess bg=new bgm=other][地]two[c]".into()),
    ]), "scenario/main/main.ks").unwrap();
    runtime.start().unwrap();
    assert_eq!(runtime.variables["f.HSceneCheck"], json!(true));
    assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
    assert_eq!(runtime.state.scenario, "scenario/main/sub.ks");
    assert_eq!(runtime.variables["f.HSceneCheck"], json!(false));
    assert_eq!(runtime.variables["f.ADV_SerifLineCount"], json!(1));
}

#[test]
fn whiteout_restores_the_final_background_without_losing_other_layers() {
    let mut runtime = vm(
        "[SC_StartProcess bg=old bgm=theme ms=3][stc ch=1 f=111][地]one[c]\n[whiteout bg=final][地]two[c]",
    );
    runtime.start().unwrap();
    let sprites = runtime.state.sprites.clone();
    runtime.take_effects();
    assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
    assert_eq!(runtime.state.background.as_deref(), Some("final"));
    assert_eq!(runtime.state.sprites, sprites);
    assert_eq!(runtime.state.bgm.as_deref(), Some("theme"));
    assert_eq!(runtime.state.message_style, 3);
    let mut invalid = vm("[whiteout][地]not reached[c]");
    assert_eq!(invalid.start().unwrap(), Event::Blocked);
    assert!(
        invalid
            .state
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("whiteout needs its final background")
    );
}

#[test]
fn unsupported_scene_replay_does_not_guess_a_jump_or_partially_reset_the_scene() {
    for command in [
        "SC_SceneStartProcess bg=new bgm=other",
        "SC_SceneEnd",
        "SC_EndProcess",
    ] {
        let source = format!(
            "[SC_StartProcess bg=old bgm=theme][stc ch=1 f=111][SC_SceneStartProcess][地]one[c]\n[eval exp='tf.SCENE_Enabled = true'][{command}][地]not reached[c]"
        );
        let mut runtime = vm(&source);
        runtime.start().unwrap();
        let sprites = runtime.state.sprites.clone();
        assert_eq!(runtime.advance().unwrap(), Event::Blocked);
        assert!(
            runtime
                .state
                .diagnostic
                .as_deref()
                .unwrap()
                .contains("scene-replay navigation")
        );
        assert_eq!(runtime.variables["f.HSceneCheck"], json!(true));
        assert_eq!(runtime.state.background.as_deref(), Some("old"));
        assert_eq!(runtime.state.bgm.as_deref(), Some("theme"));
        assert_eq!(runtime.state.sprites, sprites);
    }
}

fn legacy_stop(runtime: &mut Runtime, diagnostic: &str) {
    let mut snapshot = runtime.snapshot();
    snapshot.started = true;
    snapshot.state.scenario = "main.ks".into();
    snapshot.state.source_line = 1;
    snapshot.state.waiting = true;
    snapshot.state.diagnostic = Some(diagnostic.into());
    // A legacy blocked save may predate these native bookkeeping variables.
    snapshot.variables.remove("tf.SCENE_Enabled");
    snapshot.variables.remove("f.HSceneCheck");
    runtime.restore(snapshot).unwrap();
}

#[test]
fn an_old_unknown_scene_command_save_retries_the_exact_instruction_after_upgrade() {
    let mut runtime = vm("[SC_SceneStartProcess][地]resumed[c]");
    legacy_stop(
        &mut runtime,
        "main.ks:1: unknown command [SC_SceneStartProcess]; no implicit no-op or guessed branch",
    );
    assert_eq!(runtime.location().pc, 0);
    assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
    assert!(runtime.state.diagnostic.is_none());
    assert_eq!(runtime.state.text, "resumed");
    assert_eq!(runtime.variables["f.HSceneCheck"], json!(true));
}

#[test]
fn legacy_retry_never_discards_unrelated_unknown_commands_or_new_argument_errors() {
    for (source, diagnostic) in [
        (
            "[unknown_semantic][地]not reached[c]",
            "main.ks:1: unknown command [SC_SceneStartProcess]; no implicit no-op or guessed branch",
        ),
        (
            "[SC_SceneStartProcess][地]not reached[c]",
            "main.ks:1: invalid scene state",
        ),
        (
            "[SC_SceneStartProcess][地]not reached[c]",
            "main.ks:9: unknown command [SC_SceneStartProcess]; no implicit no-op or guessed branch",
        ),
    ] {
        let mut runtime = vm(source);
        legacy_stop(&mut runtime, diagnostic);
        let location = runtime.location().clone();
        assert_eq!(runtime.advance().unwrap(), Event::Blocked);
        assert_eq!(runtime.location(), &location);
        assert_eq!(runtime.state.diagnostic.as_deref(), Some(diagnostic));
    }
    let mut invalid = vm("[whiteout][地]not reached[c]");
    legacy_stop(
        &mut invalid,
        "main.ks:1: unknown command [whiteout]; no implicit no-op or guessed branch",
    );
    assert_eq!(invalid.advance().unwrap(), Event::Blocked);
    assert_eq!(invalid.location().pc, 0);
    assert!(
        invalid
            .state
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("whiteout needs its final background")
    );
    assert_eq!(invalid.advance().unwrap(), Event::Blocked);
}
