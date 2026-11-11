//! Read-only integration checks for the user's original XP3 installation.
//! Set KOIKARI_ASSET_ROOT to test another installation. Without it, these tests
//! use the directory above CARGO_MANIFEST_DIR and skip if archives are absent.
//! They read indexes and a few selected assets, never scan/bulk-extract 2.9 GB.

use koikari_metal::archive::Xp3Archive;
use koikari_metal::assets::AssetStore;
use std::path::{Path, PathBuf};

fn original_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("KOIKARI_ASSET_ROOT") {
        let root = PathBuf::from(root);
        assert!(
            root.join("data.xp3").is_file(),
            "KOIKARI_ASSET_ROOT must contain data.xp3"
        );
        assert!(
            root.join("patch.xp3").is_file(),
            "KOIKARI_ASSET_ROOT must contain patch.xp3"
        );
        return Some(root);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .to_path_buf();
    if root.join("data.xp3").is_file() && root.join("patch.xp3").is_file() {
        Some(root)
    } else {
        eprintln!(
            "Skipping original-archive integration check: set KOIKARI_ASSET_ROOT to the game directory"
        );
        None
    }
}

fn file_identity(path: &Path) -> (u64, std::time::SystemTime) {
    let metadata = std::fs::metadata(path).unwrap();
    (metadata.len(), metadata.modified().unwrap())
}

#[test]
fn original_index_layout_names_offsets_and_statistics() {
    let Some(root) = original_root() else { return };
    let data_path = root.join("data.xp3");
    let patch_path = root.join("patch.xp3");
    let before = (file_identity(&data_path), file_identity(&patch_path));
    let data = Xp3Archive::open(&data_path).unwrap();
    let patch = Xp3Archive::open(&patch_path).unwrap();
    assert_eq!(data.stats().archive_bytes, 2_899_957_471);
    assert_eq!(data.stats().entry_count, 15_051);
    assert_eq!(data.stats().index_stored_bytes, 607_904);
    assert_eq!(data.stats().index_original_bytes, 2_714_750);
    assert_eq!(patch.stats().archive_bytes, 2_727_999);
    assert_eq!(patch.stats().entry_count, 20);
    assert_eq!(patch.stats().index_stored_bytes, 679);
    assert_eq!(patch.stats().index_original_bytes, 2_886);
    assert_eq!(data.stats().index_blocks, 1);
    assert_eq!(patch.stats().index_blocks, 1);
    let movie = data.entry("bgimage/OPmovie.mp4").unwrap().unwrap();
    assert_eq!(movie.original_size, 253_164_627);
    assert!(movie.segments.iter().all(|s| s.offset >= 19));
    assert!(
        data.entry("UI\\SOURCEHANSANSSC-BOLD.OTF")
            .unwrap()
            .is_some()
    );
    let image = patch.entry("data/evimage/ev703_1.webp").unwrap().unwrap();
    assert_eq!(image.original_size, 2_686_708);
    assert_eq!(image.segments[0].flags, 0);
    assert_eq!(image.segments[0].offset, 25_846);
    assert_eq!(image.segments[0].original_size, 2_686_708);
    assert_eq!(image.segments[0].stored_size, 2_686_708);
    assert_eq!(
        before,
        (file_identity(&data_path), file_identity(&patch_path))
    );
}

#[test]
fn original_checksums_text_title_images_audio_and_patch_aliases() {
    let Some(root) = original_root() else { return };
    let data_path = root.join("data.xp3");
    let patch_path = root.join("patch.xp3");
    let before = (file_identity(&data_path), file_identity(&patch_path));
    // This historical regression deliberately checks the original two-archive
    // profile; additional archive coverage lives in archive_additional.rs.
    let store = AssetStore::open_base(&root).unwrap();
    assert_eq!(store.stats().archive_count, 2);
    assert_eq!(store.archives()[0].stats().entry_count, 15_051);
    assert_eq!(store.archives()[1].stats().entry_count, 20);
    assert!(store.fingerprint().starts_with("koikari-assets-v1-"));
    let config = store.text("system/config.tjs").unwrap();
    assert!(config.contains("scWidth = 1280 << 1"));
    assert!(config.contains("scHeight = 720 << 1"));
    assert!(config.contains("Source Han Sans SC Bold"));
    let title = store.read("ti_BASE_0").unwrap();
    let title = image::load_from_memory(&title).unwrap();
    assert_eq!((title.width(), title.height()), (2560, 1440));
    let logo = image::load_from_memory(&store.read("TI_タイトルロゴ").unwrap()).unwrap();
    assert_eq!((logo.width(), logo.height()), (900, 486));
    // Exact differently-prefixed base paths must not bypass the patch alias.
    let patched_image = store.archives()[1].read("ev703_1.webp").unwrap();
    assert_eq!(store.read("ev703_1").unwrap(), patched_image);
    assert_eq!(store.read("evimage/ev703_1.webp").unwrap(), patched_image);
    assert_eq!(
        store.read("data/evimage/ev703_1.webp").unwrap(),
        patched_image
    );
    for entry in store.archives()[1].entries().values() {
        if entry.name.ends_with(".ks") || entry.name.ends_with(".tjs") {
            let raw = store.archives()[1].read(&entry.name).unwrap();
            assert_eq!(store.read(&entry.name).unwrap(), raw, "{}", entry.name);
            assert!(!store.text(&entry.name).unwrap().is_empty());
        }
    }
    let audio = store.archives()[0]
        .entries()
        .values()
        .find(|entry| {
            entry.name.to_lowercase().ends_with(".ogg") && entry.original_size < 8 * 1024 * 1024
        })
        .expect("original archive should contain Ogg audio");
    let audio = store.read(&audio.name).unwrap();
    assert!(audio.starts_with(b"OggS"));
    assert_eq!(
        before,
        (file_identity(&data_path), file_identity(&patch_path))
    );
}
