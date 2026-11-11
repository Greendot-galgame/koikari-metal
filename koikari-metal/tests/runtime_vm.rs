use koikari_metal::{
    runtime::{Event, Runtime},
    save,
    script::{self, Tag, TokenKind},
};
use serde_json::json;
use std::collections::BTreeMap;

fn fixture(main: &str, sub: &str) -> Runtime {
    Runtime::from_scripts(
        BTreeMap::from([
            ("main.ks".into(), main.into()),
            ("sub.ks".into(), sub.into()),
        ]),
        "main.ks",
    )
    .unwrap()
}

#[test]
fn kag_tokenizer_is_quote_aware_and_retains_parameter_macros() {
    let source = "; not text\n*start|bookmark\n@stc ch=2 dr5 pc f=1121 s1\n[地]literal [[ bracket[eval exp=\"f.x = 'a ] = b'\"][c]";
    let tokens = script::tokenize(source).unwrap();
    assert!(matches!(&tokens[0].kind,TokenKind::Label(s) if s == "start"));
    let TokenKind::Tag(tag) = &tokens[1].kind else {
        panic!("missing directive");
    };
    assert_eq!(tag.name, "stc");
    assert_eq!(tag.get("ch"), Some("2"));
    assert_eq!(tag.flags, vec!["dr5", "pc", "s1"]);
    assert!(
        tokens
            .iter()
            .any(|t| matches!(&t.kind,TokenKind::Text(s) if s == "literal [ bracket"))
    );
    let exp = tokens
        .iter()
        .find_map(|t| match &t.kind {
            TokenKind::Tag(t) if t.name == "eval" => t.get("exp"),
            _ => None,
        })
        .unwrap();
    assert_eq!(exp, "f.x = 'a ] = b'");
    let tag: Tag =
        script::parse_tag("speaker text=\"two words \\\"quoted\\\"\" vo='voice file'").unwrap();
    assert_eq!(tag.get("text"), Some("two words \"quoted\""));
    assert_eq!(tag.get("vo"), Some("voice file"));
    assert!(script::tokenize("[eval exp=\"unterminated]").is_err());
}

#[test]
fn embedded_tjs_is_opaque_and_never_run_or_shown() {
    let source = "[iscript]\nSystem.launch('anything');\n[endscript]\n[地]unreachable[c]";
    let tokens = script::tokenize(source).unwrap();
    assert_eq!(
        tokens
            .iter()
            .filter(|t| matches!(t.kind, TokenKind::Script(_)))
            .count(),
        1
    );
    let mut runtime = fixture(source, "");
    assert_eq!(runtime.start().unwrap(), Event::Blocked);
    assert!(
        runtime
            .state
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("embedded TJS")
    );
    assert!(runtime.state.text.is_empty());
}

#[test]
fn safe_expressions_support_checked_arithmetic_and_atomic_assignments() {
    let mut vars = BTreeMap::new();
    script::assign("f.n = 2; f.n += 3; tf.label = 'route' + f.n", &mut vars).unwrap();
    assert_eq!(vars["f.n"], json!(5));
    assert_eq!(vars["tf.label"], json!("route5"));
    assert_eq!(
        script::evaluate("(f.n >= 5 && !(f.n == 4)) || false", &vars).unwrap(),
        json!(true)
    );
    assert_eq!(
        script::evaluate("[1, -3 << 1, true, 'x']", &vars).unwrap(),
        json!([1, -6, true, "x"])
    );
    let before = vars.clone();
    assert!(script::assign("f.n = 9; f.bad = System.read()", &mut vars).is_err());
    assert_eq!(vars, before);
    assert!(script::evaluate("f.uninitialized == 0", &vars).is_err());
    assert!(script::evaluate("1 / 0", &vars).is_err());
    assert!(script::assign("System.foo = 1", &mut vars).is_err());
}

#[test]
fn nested_branches_calls_and_return_follow_explicit_targets() {
    let main = "[eval exp=\"f.n = 0\"][call storage='sub.ks' target='*work'][if exp='f.n == 2'][if exp='true'][地]correct[c][else][地]wrong-inner[c][endif][elsif exp='f.n == 3'][地]wrong-elsif[c][else][地]wrong-else[c][endif][地]after[c][return]";
    let sub = "[jump target='*other']\n*work\n[eval exp='f.n += 2'][return]\n*other\n[eval exp='f.n = 9'][return]";
    let mut runtime = fixture(main, sub);
    assert_eq!(runtime.start().unwrap(), Event::Dialogue);
    assert_eq!(runtime.state.text, "correct");
    assert_eq!(runtime.variables["f.n"], json!(2));
    assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
    assert_eq!(runtime.state.text, "after");
    assert_eq!(runtime.advance().unwrap(), Event::Ended);
}

#[test]
fn unresolved_conditions_and_unknown_commands_pause_without_guessing() {
    for source in [
        "[if exp='f.missing'][地]yes[c][else][地]no[c][endif]",
        "[unknown_semantic][地]not reached[c]",
    ] {
        let mut runtime = fixture(source, "");
        assert_eq!(runtime.start().unwrap(), Event::Blocked);
        let pc = runtime.location().clone();
        assert!(runtime.state.text.is_empty());
        assert!(runtime.state.diagnostic.is_some());
        assert_eq!(runtime.advance().unwrap(), Event::Blocked);
        assert_eq!(runtime.location(), &pc);
    }
}

#[test]
fn suspension_does_not_automatically_choose_and_vm_has_step_limit() {
    let mut runtime = fixture("[s][地]not reached[c]", "");
    assert_eq!(runtime.start().unwrap(), Event::Blocked);
    let mut runtime = fixture("*forever\n[jump target='*forever']", "");
    assert_eq!(runtime.start().unwrap(), Event::Blocked);
    assert!(
        runtime
            .state
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("instruction budget")
    );
}

#[test]
fn generic_choices_wait_and_take_only_the_requested_branch() {
    let source = "[choice text='one' target='*one'][choice text='two' target='*two'][s]\n*one\n[地]ONE[c][return]\n*two\n[地]TWO[c][return]";
    let mut runtime = fixture(source, "");
    assert_eq!(runtime.start().unwrap(), Event::Choices);
    assert_eq!(runtime.advance().unwrap(), Event::Choices);
    assert_eq!(runtime.state.choices.len(), 2);
    assert!(runtime.choose(9).is_err());
    assert_eq!(runtime.choose(1).unwrap(), Event::Dialogue);
    assert_eq!(runtime.state.text, "TWO");
}

#[test]
fn snapshot_resumes_inside_call_with_identical_future_state() {
    let main = "[eval exp='f.n = 1'][call storage='sub.ks' target='*work'][地]done[emb exp='f.n'][c][return]";
    let sub = "*work\n[地]inside[c][eval exp='f.n += 4'][return]";
    let mut original = fixture(main, sub);
    original.start().unwrap();
    original.take_effects();
    assert_eq!(original.state.text, "inside");
    let snapshot = save::from_json(&save::to_json(&original.snapshot()).unwrap()).unwrap();
    assert_eq!(snapshot.call_stack.len(), 1);
    let mut resumed = fixture(main, sub);
    resumed.restore(snapshot.clone()).unwrap();
    resumed.take_effects();
    assert_eq!(resumed.snapshot(), snapshot);
    assert_eq!(original.advance().unwrap(), resumed.advance().unwrap());
    assert_eq!(original.state, resumed.state);
    assert_eq!(original.snapshot(), resumed.snapshot());
    assert_eq!(resumed.state.text, "done5");
    let mut changed = fixture(main, "*work\n[地]changed[c][return]");
    assert!(changed.restore(snapshot).is_err());
}

#[test]
fn malformed_snapshots_fail_without_replacing_live_state() {
    let mut runtime = fixture("[地]one[c][地]two[c]", "");
    runtime.start().unwrap();
    let before = runtime.snapshot();
    let mut invalid = before.clone();
    invalid.location.pc = usize::MAX;
    assert!(runtime.restore(invalid).is_err());
    assert_eq!(runtime.state, before.state);
    let mut invalid = before.clone();
    invalid.version += 1;
    assert!(runtime.restore(invalid).is_err());
    assert_eq!(runtime.state, before.state);
    assert!(save::from_json("{broken").is_err());
}

#[test]
fn face_portraits_replace_the_previous_character_without_clearing_other_stands() {
    let source = "[stc ch=1 f=111 pl][stc ch=6 f=1124 fc][地]a[c]\n[stc ch=7 f=1127 fc][地]b[c]\n[stc ch=7 f=111 pr][地]c[c]\n[h7][地]d[c]";
    let mut runtime = fixture(source, "");
    runtime.start().unwrap();
    assert_eq!(runtime.state.sprites.len(), 2);
    assert_eq!(
        runtime.state.sprites.iter().filter(|s| s.portrait).count(),
        1
    );
    assert_eq!(
        runtime
            .state
            .sprites
            .iter()
            .find(|s| s.portrait)
            .unwrap()
            .id,
        6
    );
    runtime.advance().unwrap();
    assert_eq!(runtime.state.sprites.len(), 2);
    assert_eq!(
        runtime.state.sprites.iter().filter(|s| s.portrait).count(),
        1
    );
    let portrait = runtime.state.sprites.iter().find(|s| s.portrait).unwrap();
    assert_eq!(portrait.id, 7);
    assert_eq!(portrait.storage, "stb7_s3_1_1");
    assert_eq!(portrait.overlays[0], "st7_s3_1_27");
    assert!(
        runtime
            .state
            .sprites
            .iter()
            .any(|s| s.id == 1 && !s.portrait)
    );
    runtime.advance().unwrap();
    assert_eq!(runtime.state.sprites.len(), 2);
    assert!(runtime.state.sprites.iter().all(|s| !s.portrait));
    runtime.advance().unwrap();
    assert_eq!(runtime.state.sprites.len(), 1);
    assert_eq!(runtime.state.sprites[0].id, 1);
}

#[test]
fn legacy_saves_migrate_overlapping_faces_to_one_foreground_portrait() {
    let source = "[stc ch=6 f=1124 fc][地]a[c]";
    let mut runtime = fixture(source, "");
    runtime.start().unwrap();
    let mut snapshot = runtime.snapshot();
    let mut newer = snapshot.state.sprites[0].clone();
    newer.id = 7;
    newer.storage = "stb7_s3_1_1".into();
    newer.overlays = vec!["st7_s3_1_27".into()];
    snapshot.state.sprites.push(newer);
    // Serialize the historical v1 representation, which had no portrait field.
    let mut legacy = serde_json::to_value(&snapshot).unwrap();
    for sprite in legacy["state"]["sprites"].as_array_mut().unwrap() {
        sprite.as_object_mut().unwrap().remove("portrait");
    }
    let legacy = serde_json::from_value(legacy).unwrap();
    runtime.restore(legacy).unwrap();
    assert_eq!(runtime.state.sprites.len(), 1);
    assert!(runtime.state.sprites[0].portrait);
    assert_eq!(runtime.state.sprites[0].id, 7);
    let roundtrip = save::from_json(&save::to_json(&runtime.snapshot()).unwrap()).unwrap();
    runtime.restore(roundtrip).unwrap();
    assert_eq!(runtime.state.sprites.len(), 1);
    assert!(runtime.state.sprites[0].portrait);
}

#[test]
fn image_layer_keeps_authored_z_index() {
    let mut runtime = fixture(
        "[image layer=49 storage='effect' left=12 top=34 index=99999][地]a[c]",
        "",
    );
    runtime.start().unwrap();
    let sprite = &runtime.state.sprites[0];
    assert_eq!((sprite.x, sprite.y, sprite.z), (12.0, 34.0, 99999));
    assert!(!sprite.portrait);
}

#[test]
fn atomic_save_roundtrip() {
    let mut runtime = fixture("[地]one[c][地]two[c]", "");
    runtime.start().unwrap();
    let path = std::env::temp_dir().join(format!(
        "koikari-save-test-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    runtime.save(&path).unwrap();
    runtime.advance().unwrap();
    runtime.load(&path).unwrap();
    assert_eq!(runtime.state.text, "one");
    assert_eq!(runtime.advance().unwrap(), Event::Dialogue);
    assert_eq!(runtime.state.text, "two");
    std::fs::remove_file(path).unwrap();
}
