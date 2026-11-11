//! Pure presentation geometry, shared by drawing and mouse hit-testing.
//!
//! Winit cursor positions and inner_size() are both physical pixels. Apply this
//! one letterbox transform to both; never multiply either by scale_factor().
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    canvas: (f32, f32),
    surface: (f32, f32),
    scale: f32,
    offset_x: f32,
    offset_y: f32,
}

impl Viewport {
    pub fn new(canvas: (f32, f32), surface: (f32, f32)) -> Self {
        let dimension = |value: f32| {
            if value.is_finite() && value > 0.0 {
                value
            } else {
                1.0
            }
        };
        let canvas = (dimension(canvas.0), dimension(canvas.1));
        let surface = (dimension(surface.0), dimension(surface.1));
        let scale = (surface.0 / canvas.0).min(surface.1 / canvas.1);
        Self {
            canvas,
            surface,
            scale,
            offset_x: (surface.0 - canvas.0 * scale) * 0.5,
            offset_y: (surface.1 - canvas.1 * scale) * 0.5,
        }
    }

    pub fn canvas_to_surface(&self, x: f32, y: f32) -> (f32, f32) {
        (
            x * self.scale + self.offset_x,
            y * self.scale + self.offset_y,
        )
    }

    /// Returns None in the letterbox bars or for invalid pointer coordinates.
    pub fn surface_to_canvas(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let x = (x - self.offset_x) / self.scale;
        let y = (y - self.offset_y) / self.scale;
        if x < 0.0 || y < 0.0 || x > self.canvas.0 || y > self.canvas.1 {
            None
        } else {
            Some((x, y))
        }
    }

    pub fn ndc(&self, x: f32, y: f32) -> [f32; 2] {
        let (px, py) = self.canvas_to_surface(x, y);
        [
            px / self.surface.0 * 2.0 - 1.0,
            1.0 - py / self.surface.1 * 2.0,
        ]
    }
}

pub fn contains(rect: [f32; 4], point: (f32, f32)) -> bool {
    let [x, y, width, height] = rect;
    point.0 >= x && point.0 < x + width && point.1 >= y && point.1 < y + height
}

/// Fit every choice on the authored canvas rather than letting later rows run
/// off the bottom. Drawing and hit-testing must call this exact same function.
pub fn choice_rect(index: usize, count: usize, canvas: (f32, f32)) -> Option<[f32; 4]> {
    if index >= count || count == 0 {
        return None;
    }
    let width = 1500.0_f32.min((canvas.0 - 160.0).max(1.0));
    let available = (canvas.1 - 320.0).max(1.0);
    let gap = 36.0_f32.min(available / count as f32 * 0.25);
    let height = 104.0_f32.min((available - gap * (count - 1) as f32) / count as f32);
    let total = height * count as f32 + gap * (count - 1) as f32;
    Some([
        (canvas.0 - width) * 0.5,
        (canvas.1 - total) * 0.5 + index as f32 * (height + gap),
        width,
        height,
    ])
}

pub fn choice_at(point: (f32, f32), count: usize, canvas: (f32, f32)) -> Option<usize> {
    (0..count)
        .find(|&index| choice_rect(index, count, canvas).is_some_and(|rect| contains(rect, point)))
}

pub fn title_menu_rect(index: usize, canvas: (f32, f32)) -> [f32; 4] {
    [
        (canvas.0 - 900.0) * 0.5,
        760.0 + index as f32 * 132.0,
        900.0,
        100.0,
    ]
}

pub fn show_dialogue(
    message_visible: bool,
    hide_message: bool,
    has_line: bool,
    choice_count: usize,
) -> bool {
    message_visible && !hide_message && has_line && choice_count == 0
}

/// Reserve the occupied left portion of the message window for its foreground
/// portrait, while keeping enough space for readable dialogue on narrow layouts.
pub fn message_text_left(
    default_left: f32,
    right: f32,
    message_top: f32,
    portraits: &[[f32; 4]],
) -> f32 {
    portraits
        .iter()
        .filter(|rect| {
            rect[2] > 0.0
                && rect[3] > 0.0
                && rect[0] < right * 0.5
                && rect[1] + rect[3] > message_top
        })
        .map(|rect| rect[0] + rect[2] + 24.0)
        .fold(default_left, f32::max)
        .min((right - 320.0).max(default_left))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: (f32, f32) = (2560.0, 1440.0);

    fn near(a: f32, b: f32) {
        assert!((a - b).abs() < 0.002, "{a} != {b}");
    }

    #[test]
    fn all_choice_centers_are_clickable_on_resized_retina_and_letterboxed_surfaces() {
        // These are physical sizes: 1280x720 @1x and @2x, a resized
        // window, a square window, and an ultrawide surface.
        for surface in [
            (1280.0, 720.0),
            (2560.0, 1440.0),
            (1920.0, 1080.0),
            (1800.0, 1800.0),
            (3440.0, 1440.0),
        ] {
            let viewport = Viewport::new(CANVAS, surface);
            for count in 1..=16 {
                for index in 0..count {
                    let [x, y, w, h] = choice_rect(index, count, CANVAS).unwrap();
                    let center = (x + w * 0.5, y + h * 0.5);
                    let physical = viewport.canvas_to_surface(center.0, center.1);
                    let pointer = viewport.surface_to_canvas(physical.0, physical.1).unwrap();
                    assert_eq!(choice_at(pointer, count, CANVAS), Some(index));
                }
            }
        }
    }

    #[test]
    fn every_choice_fits_canvas_and_rows_never_overlap() {
        for count in 1..=32 {
            let mut bottom = 0.0;
            for index in 0..count {
                let [x, y, width, height] = choice_rect(index, count, CANVAS).unwrap();
                assert!(x >= 0.0 && x + width <= CANVAS.0);
                assert!(y >= bottom && y + height <= CANVAS.1);
                assert!(width > 0.0 && height > 0.0);
                bottom = y + height;
            }
        }
        assert!(choice_rect(0, 0, CANVAS).is_none());
        assert!(choice_rect(2, 2, CANVAS).is_none());
    }

    #[test]
    fn bars_and_choice_gaps_do_not_activate_buttons() {
        let wide = Viewport::new(CANVAS, (3440.0, 1440.0));
        assert!(wide.surface_to_canvas(20.0, 720.0).is_none());
        assert!(wide.surface_to_canvas(3420.0, 720.0).is_none());
        let tall = Viewport::new(CANVAS, (1800.0, 1800.0));
        assert!(tall.surface_to_canvas(900.0, 20.0).is_none());
        assert!(tall.surface_to_canvas(f32::NAN, 720.0).is_none());
        let a = choice_rect(0, 2, CANVAS).unwrap();
        let b = choice_rect(1, 2, CANVAS).unwrap();
        assert!(choice_at((1280.0, (a[1] + a[3] + b[1]) * 0.5), 2, CANVAS).is_none());
    }

    #[test]
    fn viewport_roundtrips_and_ndc_preserve_authored_geometry() {
        for surface in [(2560.0, 1440.0), (1800.0, 1800.0), (3440.0, 1440.0)] {
            let viewport = Viewport::new(CANVAS, surface);
            for (x, y) in [(0.0, 0.0), (1280.0, 720.0), (2560.0, 1440.0)] {
                let physical = viewport.canvas_to_surface(x, y);
                let canvas = viewport.surface_to_canvas(physical.0, physical.1).unwrap();
                near(canvas.0, x);
                near(canvas.1, y);
            }
            let center = viewport.ndc(1280.0, 720.0);
            near(center[0], 0.0);
            near(center[1], 0.0);
        }
    }

    #[test]
    fn dialogue_and_portrait_hide_together_for_choices_hidden_window_and_empty_lines() {
        assert!(show_dialogue(true, false, true, 0));
        assert!(!show_dialogue(false, false, true, 0));
        assert!(!show_dialogue(true, true, true, 0));
        assert!(!show_dialogue(true, false, false, 0));
        assert!(!show_dialogue(true, false, true, 2));
    }

    #[test]
    fn left_portrait_reserves_space_for_speaker_and_body_without_covering_text() {
        let right = 2390.0;
        assert_eq!(message_text_left(170.0, right, 1080.0, &[]), 170.0);
        assert_eq!(
            message_text_left(170.0, right, 1080.0, &[[0.0, 1040.0, 512.0, 400.0]]),
            536.0
        );
        assert_eq!(
            message_text_left(170.0, right, 1080.0, &[[0.0, 0.0, 512.0, 400.0]]),
            170.0
        );
        assert!(
            right - message_text_left(170.0, right, 1080.0, &[[0.0, 1040.0, 3000.0, 400.0]])
                >= 320.0
        );
    }

    #[test]
    fn title_buttons_use_the_same_physical_transform() {
        let viewport = Viewport::new(CANVAS, (3200.0, 1800.0));
        for index in 0..3 {
            let rect = title_menu_rect(index, CANVAS);
            let physical =
                viewport.canvas_to_surface(rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5);
            let point = viewport.surface_to_canvas(physical.0, physical.1).unwrap();
            assert!((0..3).find(|&i| contains(title_menu_rect(i, CANVAS), point)) == Some(index));
        }
    }
}
