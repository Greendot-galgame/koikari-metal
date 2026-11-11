//! Text shaping and rasterisation.
//!
//! The original game ships Source Han Sans `.otf` inside the archive. We ask
//! AppKit for the default CJK-capable family so Simplified Chinese, Japanese and
//! Latin all render without shipping a second copy of the font.

use std::ffi::{CString, c_char, c_void};

unsafe extern "C" {
    fn km_text_rgba(
        utf8: *const c_char,
        width: u32,
        height: u32,
        font_size: f32,
        red: f32,
        green: f32,
        blue: f32,
        alpha: f32,
        bold: i32,
        centered: i32,
    ) -> *mut u8;
    fn km_free_text(pixels: *mut u8);
    fn km_activate();
}

/// Bring the process to the foreground like a normal desktop app.
pub fn activate_application() {
    // SAFETY: the shim only touches AppKit singletons on the calling thread.
    unsafe { km_activate() }
}

/// An RGBA8 bitmap produced by the platform text engine.
pub struct TextBitmap {
    pub width: u32,
    pub height: u32,
    /// Straight (non-premultiplied) RGBA, row-major, top-left origin.
    pub rgba: Vec<u8>,
}

impl TextBitmap {
    /// Multiply RGB by alpha so the result can be blended with
    /// `source = one, destination = one - sourceAlpha`.
    pub fn into_premultiplied(mut self) -> Self {
        for pixel in self.rgba.chunks_exact_mut(4) {
            let a = pixel[3] as u16;
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u16 * a + 127) / 255) as u8;
            }
        }
        self
    }
}

/// Rasterise `text` into a `width` x `height` transparent RGBA buffer.
///
/// Returns `None` for empty text, oversized surfaces, or when the platform
/// rasteriser refuses the request; callers treat that as "nothing to draw".
pub fn rasterize(
    text: &str,
    width: u32,
    height: u32,
    font_size: f32,
    color: [f32; 4],
    bold: bool,
    centered: bool,
) -> Option<TextBitmap> {
    if text.is_empty() || width == 0 || height == 0 {
        return None;
    }
    let c_text = CString::new(text).ok()?;
    // SAFETY: `c_text` outlives the call; the returned buffer is owned by the
    // shim and released exactly once below.
    let raw = unsafe {
        km_text_rgba(
            c_text.as_ptr(),
            width,
            height,
            font_size,
            color[0],
            color[1],
            color[2],
            color[3],
            i32::from(bold),
            i32::from(centered),
        )
    };
    if raw.is_null() {
        return None;
    }
    let len = width as usize * height as usize * 4;
    // SAFETY: the shim allocated `len` bytes via calloc.
    let rgba = unsafe { std::slice::from_raw_parts(raw as *const u8, len) }.to_vec();
    unsafe { km_free_text(raw as *mut u8) };
    Some(TextBitmap { width, height, rgba })
}

/// Draw a `c_void`-free debugging helper for tests.
#[allow(dead_code)]
fn _assert_no_extern_globals(_: *mut c_void) {}
