//! Independent audit regressions using only synthetic scripts and in-memory JSON.
//! No archives, media, original TJS/DLL, or filesystem save data is accessed.
use koikari_metal::{
    runtime::{Event, Runtime, Snapshot},
    save, script,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
};

fn vm(source: &str) -> Runtime {
    Runtime::from_scripts(
        BTreeMap::from([("main.ks".into(), source.into())]),
        "main.ks",
    )
    .unwrap()
}

fn json_roundtrip(snapshot: &Snapshot) -> Snapshot {
    save::from_json(&save::to_json(snapshot).unwrap()).unwrap()
}

#[test]
fn failed_restore_does_not_add_dependencies_to_the_live_snapshot() {
    let main = "[地]one[c][地]two[c]";
    let unused = "[地]unused[c]";
    let mut runtime = Runtime::from_scripts(
        BTreeMap::from([
            ("main.ks".into(), main.into()),
            ("unused.ks".into(), unused.into()),
        ]),
        "main.ks",
    )
    .unwrap();
    assert_eq!(runtime.start().unwrap(), Event::Dialogue);
    runtime.take_effects();
    let before = runtime.snapshot();
    assert!(!before.source_fingerprints.contains_key("unused.ks"));

    let mut invalid = before.clone();
    invalid
        .source_fingerprints
        .insert("unused.ks".into(), script::fingerprint(unused));
    invalid.location.pc = usize::MAX;
    let error = runtime.restore(json_roundtrip(&invalid)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("program counter is out of range")
    );
    assert!(runtime.take_effects().is_empty());
    assert_eq!(
        runtime.snapshot(),
        before,
        "a rejected restore must not add unrelated scenario identities to later saves"
    );
}

#[test]
fn restoring_an_older_snapshot_does_not_keep_later_cached_dependencies() {
    let scripts: BTreeMap<String, String> = BTreeMap::from([
        (
            "main.ks".into(),
            "[地]one[c][jump storage='later.ks']".into(),
        ),
        ("later.ks".into(), "[地]later[c]".into()),
    ]);
    let mut runtime = Runtime::from_scripts(scripts, "main.ks").unwrap();
    assert_eq!(runtime.start().unwrap(), Event::Dialogue);
    let earlier = runtime.snapshot();
    assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
    assert!(
        runtime
            .snapshot()
            .source_fingerprints
            .contains_key("later.ks")
    );

    runtime.restore(json_roundtrip(&earlier)).unwrap();
    assert_eq!(
        runtime.snapshot(),
        earlier,
        "restoring an older state must restore its dependency set, not the later cache contents"
    );
}

#[test]
fn json_max_message_count_is_rejected_or_handled_without_overflow() {
    let mut runtime = vm("[地]one[c][地]two[c]");
    assert_eq!(runtime.start().unwrap(), Event::Dialogue);
    let mut encoded = serde_json::to_value(runtime.snapshot()).unwrap();
    encoded["state"]["messages_seen"] = json!(u64::MAX);
    let snapshot = save::from_json(&encoded.to_string()).unwrap();
    if runtime.restore(snapshot).is_err() {
        return;
    }

    let outcome = catch_unwind(AssertUnwindSafe(|| runtime.advance()));
    assert!(
        outcome.is_ok(),
        "an accepted save must not panic at the next dialogue boundary"
    );
    assert_eq!(
        runtime.state.messages_seen,
        u64::MAX,
        "an accepted maximum counter must not wrap to zero in release builds"
    );
}

#[test]
fn a_jump_at_the_valid_maximum_call_depth_does_not_require_another_frame() {
    let main = "[eval exp='f.depth = 0'][call storage='sub.ks'][地]done[c][return]";
    let sub = "[eval exp='f.depth += 1'][if exp='f.depth < 64'][call storage='sub.ks'][else][jump target='*leaf'][endif][return]\n*leaf\n[地]leaf[c][return]";
    let scripts: BTreeMap<String, String> = BTreeMap::from([
        ("main.ks".into(), main.into()),
        ("sub.ks".into(), sub.into()),
    ]);
    let mut runtime = Runtime::from_scripts(scripts.clone(), "main.ks").unwrap();
    let event = runtime.start().unwrap();
    assert_eq!(
        event,
        Event::Dialogue,
        "a jump does not grow the stack: {:?}",
        runtime.state.diagnostic
    );
    assert_eq!(runtime.state.text, "leaf");
    assert_eq!(runtime.snapshot().call_stack.len(), 64);

    let mut restored = Runtime::from_scripts(scripts, "main.ks").unwrap();
    restored
        .restore(json_roundtrip(&runtime.snapshot()))
        .unwrap();
    assert_eq!(restored.advance().unwrap(), Event::Dialogue);
    assert_eq!(restored.state.text, "done");
    assert!(restored.snapshot().call_stack.is_empty());
}

#[test]
fn nonfinite_sprite_offsets_are_blocked_before_they_poison_save_json() {
    for axis in ["dx", "dy"] {
        for value in ["NaN", "inf", "-inf"] {
            let source = format!("[stc ch=1 f=111 {axis}={value}][地]one[c]");
            let mut runtime = vm(&source);
            let event = runtime.start().unwrap();
            let encoded = save::to_json(&runtime.snapshot()).unwrap();
            let decoded = save::from_json(&encoded);
            assert!(
                decoded.is_ok(),
                "to_json succeeded for {axis}={value}, but its own output was unreadable: {:?}",
                decoded.err()
            );
            assert_eq!(
                event,
                Event::Blocked,
                "non-finite {axis}={value} must not enter the live scene"
            );
            assert!(runtime.state.sprites.iter().all(|s| {
                s.x.is_finite() && s.y.is_finite() && s.scale.is_finite() && s.opacity.is_finite()
            }));
            // A blocked malformed instruction must still leave a readable save.
            let _ = json_roundtrip(&runtime.snapshot());
        }
    }
}

fn legacy_snapshot(runtime: &Runtime, command: &str) -> Snapshot {
    let mut snapshot = runtime.snapshot();
    snapshot.started = true;
    snapshot.state.scenario = "main.ks".into();
    snapshot.state.source_line = 1;
    snapshot.state.waiting = true;
    snapshot.state.diagnostic = Some(format!(
        "main.ks:1: unknown command [{command}]; no implicit no-op or guessed branch"
    ));
    snapshot.variables.remove("tf.SCENE_Enabled");
    snapshot.variables.remove("f.HSceneCheck");
    json_roundtrip(&snapshot)
}

#[test]
fn legacy_unknown_retry_is_limited_to_the_three_upgraded_scene_commands() {
    for (tag, command) in [
        ("SC_SceneStartProcess", "SC_SceneStartProcess"),
        ("SC_SceneEnd", "SC_SceneEnd"),
        ("whiteout bg=final", "whiteout"),
    ] {
        let mut runtime = vm(&format!("[{tag}][地]resumed[c]"));
        runtime.restore(legacy_snapshot(&runtime, command)).unwrap();
        assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
        assert_eq!(runtime.state.text, "resumed");
        assert!(runtime.state.diagnostic.is_none());
        assert_eq!(runtime.history.len(), 1);
    }

    for command in ["unimplemented", "SC_EndProcess"] {
        let mut runtime = vm(&format!("[{command}][地]not reached[c]"));
        runtime.restore(legacy_snapshot(&runtime, command)).unwrap();
        let before = runtime.snapshot();
        assert_eq!(runtime.advance().unwrap(), Event::Blocked);
        assert_eq!(runtime.snapshot(), before);
    }
}

#[test]
fn legacy_unknown_retry_preserves_new_condition_and_argument_failures() {
    for tag in [
        "SC_SceneStartProcess cond='f.missing'",
        "SC_SceneStartProcess bg='&f.missing'",
        "whiteout",
    ] {
        let command = tag.split_whitespace().next().unwrap();
        let mut runtime = vm(&format!("[{tag}][地]not reached[c]"));
        runtime.restore(legacy_snapshot(&runtime, command)).unwrap();
        assert_eq!(runtime.advance().unwrap(), Event::Blocked);
        assert_eq!(runtime.location().pc, 0);
        assert!(runtime.state.text.is_empty());
        let diagnostic = runtime.state.diagnostic.as_deref().unwrap();
        assert!(!diagnostic.contains("unknown command"));
        let before = runtime.snapshot();
        assert_eq!(runtime.advance().unwrap(), Event::Blocked);
        assert_eq!(runtime.snapshot(), before);
    }
}
