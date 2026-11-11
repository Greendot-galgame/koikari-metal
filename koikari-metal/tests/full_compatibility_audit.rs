//! Read-only, non-visual audit of both supported resource profiles.
//! Dialogue is never printed; no images/audio are decoded or played here.
use koikari_metal::{
    assets::AssetStore,
    runtime::{AudioEffect, Event, Runtime},
    save, script,
};
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

fn profiles() -> Vec<(&'static str, Arc<AssetStore>)> {
    let Some(root) = std::env::var_os("KOIKARI_GAME_ROOT").map(PathBuf::from) else {
        return Vec::new();
    };
    let mut profiles = vec![("base", Arc::new(AssetStore::open_base(&root).unwrap()))];
    if root.join("adult.xp3").is_file() {
        profiles.push(("additional", Arc::new(AssetStore::open(root).unwrap())));
    }
    profiles
}

fn step(runtime: &mut Runtime, event: &Event) -> Event {
    if *event == Event::Choices {
        runtime.choose(0).unwrap()
    } else {
        runtime.advance().unwrap()
    }
}

#[derive(Default)]
struct References {
    images: BTreeSet<String>,
    mandatory_audio: BTreeSet<String>,
    voices: BTreeSet<String>,
}
impl References {
    fn audio(&mut self, effects: Vec<AudioEffect>) {
        for effect in effects {
            match effect {
                AudioEffect::Bgm { storage, .. } | AudioEffect::Se { storage, .. } => {
                    self.mandatory_audio.insert(storage);
                }
                AudioEffect::Voice { storage } => {
                    self.voices.insert(storage);
                }
                _ => {}
            }
        }
    }
    fn state(&mut self, runtime: &Runtime) {
        self.images.extend(runtime.state.background.iter().cloned());
        for sprite in &runtime.state.sprites {
            self.images.insert(sprite.storage.clone());
            self.images.extend(sprite.overlays.iter().cloned());
        }
    }
}

#[test]
fn every_chapter_in_both_profiles_advances_and_checkpoint_future_is_identical() {
    let mut errors = Vec::new();
    for (profile, assets) in profiles() {
        let mut runtime = Runtime::new(assets.clone()).unwrap();
        let mut resumed = Runtime::new(assets.clone()).unwrap();
        let chapters: Vec<_> = runtime
            .chapters()
            .iter()
            .map(|c| c.storage.clone())
            .collect();
        let mut references = References::default();
        let (mut boundaries, mut checkpoints) = (0, 0);
        for storage in &chapters {
            let mut event = runtime.start_at(storage, None).unwrap();
            for n in 0..5000 {
                if event == Event::Blocked {
                    errors.push(format!(
                        "{profile} {storage}:{}: {}",
                        runtime.state.source_line,
                        runtime.state.diagnostic.as_deref().unwrap_or("blocked")
                    ));
                    break;
                }
                if runtime.state.scenario != *storage || event == Event::Ended {
                    break;
                }
                if n == 4999 {
                    errors.push(format!("{profile} {storage}: step limit exceeded"));
                    break;
                }
                references.state(&runtime);
                references.audio(runtime.take_effects());
                if runtime.state.sprites.iter().filter(|s| s.portrait).count() > 1 {
                    errors.push(format!(
                        "{profile} {storage}:{}: overlapping portraits",
                        runtime.state.source_line
                    ));
                }
                if runtime.state.sprites.iter().any(|s| {
                    !s.x.is_finite()
                        || !s.y.is_finite()
                        || !s.scale.is_finite()
                        || s.scale <= 0.0
                        || !s.opacity.is_finite()
                        || !(0.0..=1.0).contains(&s.opacity)
                }) {
                    errors.push(format!(
                        "{profile} {storage}:{}: invalid sprite geometry",
                        runtime.state.source_line
                    ));
                }
                boundaries += 1;
                if n == 10 {
                    let encoded = save::to_json(&runtime.snapshot()).unwrap();
                    resumed.restore(save::from_json(&encoded).unwrap()).unwrap();
                    resumed.take_effects();
                    let restored = resumed.snapshot();
                    if restored != runtime.snapshot() {
                        errors.push(format!("{profile} {storage}: checkpoint state differs"));
                    }
                    let next = step(&mut runtime, &event);
                    let resumed_next = step(&mut resumed, &event);
                    if next != resumed_next
                        || runtime.state != resumed.state
                        || runtime.variables != resumed.variables
                        || runtime.location() != resumed.location()
                    {
                        errors.push(format!(
                            "{profile} {storage}: checkpoint continuation differs"
                        ));
                    }
                    let effects = runtime.take_effects();
                    if effects != resumed.take_effects() {
                        errors.push(format!(
                            "{profile} {storage}: checkpoint audio effects differ"
                        ));
                    }
                    references.audio(effects);
                    event = next;
                    checkpoints += 1;
                } else {
                    event = step(&mut runtime, &event);
                }
            }
        }
        for image in &references.images {
            if let Err(error) = assets.resolve(image) {
                errors.push(format!("{profile} unresolved image {image}: {error}"));
            }
        }
        for sound in &references.mandatory_audio {
            if let Err(error) = assets.resolve(sound) {
                errors.push(format!("{profile} unresolved music/SE {sound}: {error}"));
            }
        }
        let mut missing_voices = 0;
        for voice in &references.voices {
            match assets.resolve_optional(voice) {
                Ok(None) => missing_voices += 1,
                Ok(Some(_)) => {}
                Err(error) => {
                    errors.push(format!("{profile} invalid voice lookup {voice}: {error}"))
                }
            }
        }
        eprintln!(
            "full audit {profile}: {} chapters, {boundaries} boundaries, {checkpoints} serialized checkpoints, {} images, {} music/SE, {} voices ({missing_voices} absent)",
            chapters.len(),
            references.images.len(),
            references.mandatory_audio.len(),
            references.voices.len()
        );
    }
    assert!(
        errors.is_empty(),
        "profile audit failures:\n{}",
        errors.join("\n")
    );
}

#[test]
fn every_authored_choice_in_both_profiles_reaches_its_authored_join_or_destination() {
    let mut errors = Vec::new();
    for (profile, assets) in profiles() {
        let table = script::literal_assignment(
            &assets.text("scenario/DB/DB_EV.ks").unwrap(),
            "lf.EV_SelectListInfo",
        )
        .unwrap();
        let mut runtime = Runtime::new(assets.clone()).unwrap();
        let mut references = References::default();
        let mut branches = 0;
        for (number, row) in table.as_array().unwrap().iter().enumerate() {
            let storage = format!("scenario/main/{}.ks", row[0].as_str().unwrap());
            for (index, _) in row[4].as_array().unwrap().iter().enumerate() {
                let event = runtime
                    .start_at(&storage, Some(&format!("*SL_{number}")))
                    .unwrap();
                if event != Event::Choices {
                    errors.push(format!(
                        "{profile} selection {number}: did not reach a choice"
                    ));
                    continue;
                }
                let mut event = runtime.choose(index).unwrap();
                for n in 0..4000 {
                    references.state(&runtime);
                    references.audio(runtime.take_effects());
                    if event == Event::Blocked {
                        errors.push(format!(
                            "{profile} selection {number}/{index}: blocked at {}:{}: {:?}",
                            runtime.state.scenario,
                            runtime.state.source_line,
                            runtime.state.diagnostic
                        ));
                        break;
                    }
                    if runtime.state.scenario != storage
                        || matches!(event, Event::Choices | Event::Ended)
                    {
                        break;
                    }
                    if n == 3999 {
                        errors.push(format!(
                            "{profile} selection {number}/{index}: failed to converge"
                        ));
                        break;
                    }
                    runtime.take_effects();
                    event = runtime.advance().unwrap();
                }
                if let Some(destination) = row[1][index].as_str().filter(|s| !s.is_empty()) {
                    if !runtime
                        .state
                        .scenario
                        .ends_with(&format!("/{destination}.ks"))
                    {
                        errors.push(format!(
                            "{profile} selection {number}/{index}: wrong destination"
                        ));
                    }
                }
                branches += 1;
            }
        }
        for name in references
            .images
            .iter()
            .chain(references.mandatory_audio.iter())
        {
            if let Err(error) = assets.resolve(name) {
                errors.push(format!(
                    "{profile} unresolved branch resource {name}: {error}"
                ));
            }
        }
        for voice in &references.voices {
            if let Err(error) = assets.resolve_optional(voice) {
                errors.push(format!(
                    "{profile} invalid branch voice lookup {voice}: {error}"
                ));
            }
        }
        eprintln!(
            "full audit {profile}: {branches} authored choice branches and {} image / {} music/SE / {} voice references checked",
            references.images.len(),
            references.mandatory_audio.len(),
            references.voices.len()
        );
    }
    assert!(
        errors.is_empty(),
        "branch audit failures:\n{}",
        errors.join("\n")
    );
}
