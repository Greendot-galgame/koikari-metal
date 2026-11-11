//! Optional real-asset tests. They run automatically when the owned archives are
//! at the project parent, or when KOIKARI_GAME_ROOT selects their directory.
//! No copyrighted scenario text is embedded or printed by these tests.
use koikari_metal::{
    assets::AssetStore,
    runtime::{Event, FIRST_SCENARIO, Runtime},
    script::{self, Program},
};
use std::{path::PathBuf, sync::Arc};

fn assets() -> Option<Arc<AssetStore>> {
    let root = std::env::var_os("KOIKARI_GAME_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_owned()
        });
    if !root.join("data.xp3").is_file() {
        eprintln!("real XP3 smoke skipped: select owned archives with KOIKARI_GAME_ROOT");
        return None;
    }
    Some(Arc::new(
        AssetStore::open_base(root).expect("owned base archives should parse"),
    ))
}
fn assert_not_blocked(runtime: &Runtime, event: &Event) {
    assert_ne!(
        *event,
        Event::Blocked,
        "{}",
        runtime
            .state
            .diagnostic
            .as_deref()
            .unwrap_or("unknown VM diagnostic")
    );
}

#[test]
fn real_first_chapter_advances_hundreds_of_messages_and_resumes() {
    let Some(assets) = assets() else {
        return;
    };
    let mut runtime = Runtime::new(assets.clone()).unwrap();
    let event = runtime.start().unwrap();
    assert_not_blocked(&runtime, &event);
    assert_eq!(event, Event::Dialogue);
    assert_eq!(runtime.state.scenario, FIRST_SCENARIO);
    assert_eq!(runtime.state.background.as_deref(), Some("bg1_0"));
    assert_eq!(runtime.state.bgm.as_deref(), Some("bgm5"));
    assert_eq!(runtime.state.speaker, "咲希");
    assert_eq!(
        (runtime.state.canvas_width, runtime.state.canvas_height),
        (2560, 1440)
    );
    assert!(!runtime.state.text.is_empty());
    assert_eq!(runtime.state.sprites.len(), 1);
    assert_eq!(runtime.state.sprites[0].storage, "stb6_s2_1_1");
    assert!(assets.resolve(&runtime.state.sprites[0].storage).is_ok());
    let mut count = 1;
    while runtime.state.scenario == FIRST_SCENARIO {
        let event = runtime.advance().unwrap();
        assert_not_blocked(&runtime, &event);
        assert_ne!(
            event,
            Event::Choices,
            "first chapter unexpectedly contains a choice"
        );
        assert!(count < 3000, "first chapter did not terminate");
        count += 1;
        if count == 200 {
            let snapshot = runtime.snapshot();
            let mut resumed = Runtime::new(assets.clone()).unwrap();
            resumed.restore(snapshot).unwrap();
            assert_eq!(runtime.advance().unwrap(), resumed.advance().unwrap());
            assert_eq!(runtime.state, resumed.state);
            runtime = resumed;
            count += 1;
        }
    }
    assert!(count > 400, "did not exercise hundreds of real messages");
    assert!(
        runtime.state.scenario.ends_with("/0_2.ks"),
        "chapter order must come from authored event DB"
    );
}

#[test]
fn real_main_scenarios_compile_and_default_choice_paths_advance() {
    let Some(assets) = assets() else {
        return;
    };
    let mut runtime = Runtime::new(assets.clone()).unwrap();
    let chapters: Vec<String> = runtime
        .chapters()
        .iter()
        .map(|c| c.storage.clone())
        .collect();
    assert!(chapters.len() >= 90);
    assert!(
        chapters
            .windows(2)
            .any(|c| c[0].ends_with("/1_6.ks") && c[1].ends_with("/1_8.ks"))
    );
    let mut boundaries = 0;
    for name in &chapters {
        let source = assets.text(name).unwrap();
        Program::compile(name, &source).unwrap();
        let mut event = runtime.start_at(name, None).unwrap();
        for n in 0..4000 {
            assert_not_blocked(&runtime, &event);
            if runtime.state.scenario != *name || event == Event::Ended {
                break;
            }
            assert!(
                n < 3999,
                "scenario failed to reach authored continuation: {name}"
            );
            if event == Event::Choices {
                event = runtime.choose(0).unwrap();
            } else {
                boundaries += 1;
                event = runtime.advance().unwrap();
            }
        }
    }
    assert!(
        boundaries > 15000,
        "too little of the real scenario set was exercised"
    );
}

#[test]
fn reported_scenes_keep_complete_image_layers_and_only_one_portrait() {
    let Some(assets) = assets() else {
        return;
    };
    let mut runtime = Runtime::new(assets.clone()).unwrap();
    for (scenario, line) in [("0_1", 78), ("0_1", 2516), ("0_2", 306), ("0_2", 1246)] {
        let name = format!("scenario/main/{scenario}.ks");
        let mut event = runtime.start_at(&name, None).unwrap();
        for count in 0..4000 {
            assert_not_blocked(&runtime, &event);
            assert_eq!(runtime.state.scenario, name, "target line was not reached");
            if runtime.state.source_line >= line {
                break;
            }
            assert!(!matches!(event, Event::Choices | Event::Ended));
            assert!(count < 3999);
            event = runtime.advance().unwrap();
            runtime.take_effects();
        }
        assert_eq!(runtime.state.source_line, line);
        let bg = runtime
            .state
            .background
            .as_deref()
            .expect("background missing from scene graph");
        assets.resolve(bg).unwrap();
        for sprite in &runtime.state.sprites {
            assets.resolve(&sprite.storage).unwrap();
            assert!(
                !sprite.overlays.is_empty(),
                "standing character has no face overlay"
            );
            for overlay in &sprite.overlays {
                assets.resolve(overlay).unwrap();
            }
        }
        if scenario == "0_1" && line == 78 {
            assert_eq!(
                runtime.state.sprites.len(),
                1,
                "old speaking portrait was not cleared"
            );
            let portrait = &runtime.state.sprites[0];
            assert!(portrait.portrait);
            assert_eq!(portrait.id, 7);
            assert_eq!(portrait.overlays[0], "st7_s3_1_27");
        } else {
            assert!(runtime.state.sprites.iter().all(|s| !s.portrait));
            assert!(!runtime.state.sprites.is_empty());
        }
    }
}

#[test]
fn real_scene_images_resolve_across_all_chapters_without_stacked_portraits() {
    use std::collections::BTreeSet;
    let Some(assets) = assets() else {
        return;
    };
    let mut runtime = Runtime::new(assets.clone()).unwrap();
    let chapters: Vec<_> = runtime
        .chapters()
        .iter()
        .map(|c| c.storage.clone())
        .collect();
    let mut seen = BTreeSet::new();
    let mut missing = Vec::new();
    let mut boundaries = 0;
    for name in &chapters {
        let mut event = runtime.start_at(name, None).unwrap();
        for n in 0..4000 {
            assert_not_blocked(&runtime, &event);
            if runtime.state.scenario != *name || event == Event::Ended {
                break;
            }
            assert!(n < 3999, "scenario did not finish: {name}");
            assert!(
                runtime.state.sprites.iter().filter(|s| s.portrait).count() <= 1,
                "multiple face portraits: {name}:{}",
                runtime.state.source_line
            );
            let state = &runtime.state;
            let names = state.background.iter().chain(
                state
                    .sprites
                    .iter()
                    .flat_map(|s| std::iter::once(&s.storage).chain(s.overlays.iter())),
            );
            for storage in names {
                if seen.insert(storage.clone()) {
                    if let Err(error) = assets.resolve(storage) {
                        missing.push(format!("{name}:{} {storage}: {error}", state.source_line));
                    }
                }
            }
            boundaries += 1;
            event = if event == Event::Choices {
                runtime.choose(0).unwrap()
            } else {
                runtime.advance().unwrap()
            };
            runtime.take_effects();
        }
    }
    eprintln!(
        "image audit: {} chapters, {boundaries} scene boundaries, {} unique layer images",
        chapters.len(),
        seen.len()
    );
    assert!(
        missing.is_empty(),
        "unresolved scene images ({}):\\n{}",
        missing.len(),
        missing.join("\\n")
    );
    assert!(boundaries > 15000);
}

#[test]
fn every_authored_choice_branch_reaches_a_message_join_or_continuation() {
    let Some(assets) = assets() else {
        return;
    };
    let source = assets.text("scenario/DB/DB_EV.ks").unwrap();
    let table = script::literal_assignment(&source, "lf.EV_SelectListInfo").unwrap();
    let mut runtime = Runtime::new(assets).unwrap();
    let mut branches = 0;
    for (number, row) in table.as_array().unwrap().iter().enumerate() {
        let name = format!("scenario/main/{}.ks", row[0].as_str().unwrap());
        for (index, _) in row[4].as_array().unwrap().iter().enumerate() {
            let event = runtime
                .start_at(&name, Some(&format!("*SL_{number}")))
                .unwrap();
            assert_not_blocked(&runtime, &event);
            assert_eq!(event, Event::Choices);
            let mut event = runtime.choose(index).unwrap();
            assert_not_blocked(&runtime, &event);
            branches += 1;
            for n in 0..3000 {
                assert_not_blocked(&runtime, &event);
                if runtime.state.scenario != name || matches!(event, Event::Choices | Event::Ended)
                {
                    break;
                }
                assert!(
                    n < 2999,
                    "choice branch failed to converge: selection {number} option {index}"
                );
                event = runtime.advance().unwrap();
            }
            if let Some(destination) = row[1][index].as_str().filter(|s| !s.is_empty()) {
                assert!(
                    runtime
                        .state
                        .scenario
                        .ends_with(&format!("/{destination}.ks")),
                    "route choice {number}/{index} did not follow authored destination"
                );
            }
        }
    }
    assert_eq!(branches, 29);
}
