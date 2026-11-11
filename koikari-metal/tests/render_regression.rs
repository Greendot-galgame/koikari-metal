//! Opt-in actual Metal pixel tests, not just cache bookkeeping.
//! Run with KOIKARI_METAL_TESTS=1 on macOS with a Metal device.
#![cfg(target_os = "macos")]

use koikari_metal::{CANVAS_HEIGHT, CANVAS_WIDTH, renderer::Renderer};

fn enabled() -> bool {
    std::env::var_os("KOIKARI_METAL_TESTS").is_some_and(|value| value == "1")
}

fn pixel(pixels: &[u8], x: usize, y: usize) -> &[u8] {
    &pixels[(y * 256 + x) * 4..(y * 256 + x) * 4 + 4]
}

fn upload_and_draw(renderer: &mut Renderer, key: &str, rgba: [u8; 4], rect: [f32; 4]) {
    if !renderer.has_texture(key) {
        renderer.upload_rgba(key, 1, 1, &rgba).unwrap();
    }
    renderer.draw(Some(key), rect, 1.0);
}

fn composed_frame(renderer: &mut Renderer) -> Vec<u8> {
    renderer.begin_frame(256.0, 144.0);
    upload_and_draw(
        renderer,
        "background",
        [40, 80, 120, 255],
        [0.0, 0.0, CANVAS_WIDTH, CANVAS_HEIGHT],
    );
    let center = [640.0, 360.0, 1280.0, 720.0];
    // Premultiplied red body and green face overlay, sharing their origin.
    upload_and_draw(renderer, "body", [64, 0, 0, 128], center);
    upload_and_draw(renderer, "face", [0, 64, 0, 128], center);
    for key in ["background", "body", "face"] {
        assert!(
            renderer.has_texture(key),
            "current-frame layer evicted: {key}"
        );
    }
    renderer
        .render_offscreen(256, 144, [0.0, 0.0, 0.0, 1.0])
        .unwrap()
}

#[test]
fn metal_low_budget_frames_keep_body_face_and_background_and_reload_evicted_images() {
    if !enabled() {
        eprintln!("Metal pixel test skipped: set KOIKARI_METAL_TESTS=1");
        return;
    }
    // The budget holds only two pixels, less than the visible three-layer group.
    let mut renderer = Renderer::with_texture_budget(8).unwrap();
    let reference = composed_frame(&mut renderer);
    assert_eq!(pixel(&reference, 10, 10), &[40, 80, 120, 255]);
    let center = pixel(&reference, 128, 72);
    for (actual, expected) in center.iter().zip([42i16, 84, 30, 255]) {
        assert!(
            (i16::from(*actual) - expected).abs() <= 1,
            "bad alpha/layer order: {center:?}"
        );
    }
    for iteration in 0..20 {
        renderer.begin_frame(256.0, 144.0);
        renderer
            .upload_rgba(
                &format!("intervening-{iteration}"),
                1,
                1,
                &[255, 255, 255, 255],
            )
            .unwrap();
        assert!(
            !renderer.has_texture("background"),
            "test did not force background eviction"
        );
        let reloaded = composed_frame(&mut renderer);
        assert_eq!(
            reference, reloaded,
            "pixel loss after cache pressure at frame {iteration}"
        );
    }
}

#[test]
fn metal_foreground_portrait_is_not_tinted_by_the_message_frame() {
    if !enabled() {
        return;
    }
    let mut renderer = Renderer::with_texture_budget(8).unwrap();
    renderer.begin_frame(256.0, 144.0);
    upload_and_draw(
        &mut renderer,
        "background",
        [255, 255, 255, 255],
        [0.0, 0.0, CANVAS_WIDTH, CANVAS_HEIGHT],
    );
    upload_and_draw(
        &mut renderer,
        "message-frame",
        [0, 0, 160, 200],
        [0.0, 1080.0, CANVAS_WIDTH, 360.0],
    );
    upload_and_draw(
        &mut renderer,
        "portrait",
        [230, 180, 140, 255],
        [0.0, 1040.0, 512.0, 400.0],
    );
    let pixels = renderer
        .render_offscreen(256, 144, [0.0, 0.0, 0.0, 1.0])
        .unwrap();
    assert_eq!(pixel(&pixels, 20, 120), &[230, 180, 140, 255]);
    assert_ne!(pixel(&pixels, 128, 120), &[230, 180, 140, 255]);
}
