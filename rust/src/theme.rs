//! Colour themes — actually wired this time.
//!
//! In the Python original `themes.py` was orphaned (defined, never applied). Here
//! the render palette is a global current theme: `render`'s colour accessors read
//! `current()`, and `cycle()` (bound to `t` / a Lighting-tab button) rotates it.
//! Single global atomic index — the UI is single-threaded, so this is enough.

use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::style::Color;

#[derive(Clone, Copy)]
pub struct Theme {
    pub label: &'static str,
    pub neon: Color,
    pub cyan: Color,
    pub magenta: Color,
    pub amber: Color,
    pub red: Color,
    pub dim: Color,
    pub text: Color,
    pub blue: Color,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Palettes ported from `themes.py` (cyberpunk is the lossless default).
pub const THEMES: [Theme; 5] = [
    Theme {
        label: "Cyberpunk",
        neon: rgb(0x00, 0xff, 0x9c),
        cyan: rgb(0x36, 0xf9, 0xf6),
        magenta: rgb(0xff, 0x2e, 0x88),
        amber: rgb(0xf3, 0xd0, 0x00),
        red: rgb(0xff, 0x33, 0x55),
        dim: rgb(0x4b, 0x52, 0x63),
        text: rgb(0xc9, 0xd1, 0xd9),
        blue: rgb(0x5a, 0xc8, 0xfa),
    },
    Theme {
        label: "Synthwave",
        neon: rgb(0xff, 0x6a, 0xc1),
        cyan: rgb(0x36, 0xf9, 0xf6),
        magenta: rgb(0xb9, 0x67, 0xff),
        amber: rgb(0xff, 0xcc, 0x55),
        red: rgb(0xff, 0x4d, 0x6d),
        dim: rgb(0x5b, 0x52, 0x77),
        text: rgb(0xf0, 0xe6, 0xff),
        blue: rgb(0x7a, 0xa2, 0xff),
    },
    Theme {
        label: "Matrix",
        neon: rgb(0x00, 0xff, 0x66),
        cyan: rgb(0x33, 0xff, 0x99),
        magenta: rgb(0x88, 0xff, 0x88),
        amber: rgb(0xaa, 0xff, 0x55),
        red: rgb(0xff, 0x55, 0x55),
        dim: rgb(0x1f, 0x5f, 0x3f),
        text: rgb(0xb8, 0xff, 0xcf),
        blue: rgb(0x33, 0xff, 0xaa),
    },
    Theme {
        label: "Amber CRT",
        neon: rgb(0xff, 0xb0, 0x00),
        cyan: rgb(0xff, 0xcc, 0x66),
        magenta: rgb(0xff, 0x8c, 0x42),
        amber: rgb(0xff, 0xd7, 0x00),
        red: rgb(0xff, 0x5e, 0x3a),
        dim: rgb(0x6b, 0x4a, 0x1f),
        text: rgb(0xff, 0xe6, 0xb3),
        blue: rgb(0xff, 0xaa, 0x55),
    },
    Theme {
        label: "Ice",
        neon: rgb(0x5a, 0xc8, 0xfa),
        cyan: rgb(0x7f, 0xff, 0xd4),
        magenta: rgb(0xa0, 0xc4, 0xff),
        amber: rgb(0xff, 0xd1, 0x66),
        red: rgb(0xff, 0x6b, 0x6b),
        dim: rgb(0x46, 0x58, 0x6b),
        text: rgb(0xe0, 0xf0, 0xff),
        blue: rgb(0x4d, 0x96, 0xff),
    },
];

static IDX: AtomicUsize = AtomicUsize::new(0);

/// The active theme.
pub fn current() -> Theme {
    THEMES[IDX.load(Ordering::Relaxed) % THEMES.len()]
}

/// Advance to the next theme; returns its label.
pub fn cycle() -> &'static str {
    let n = (IDX.load(Ordering::Relaxed) + 1) % THEMES.len();
    IDX.store(n, Ordering::Relaxed);
    THEMES[n].label
}

/// The active theme's label.
pub fn current_label() -> &'static str {
    current().label
}

/// Select a theme by label (case-insensitive); no-op if unknown.
pub fn set_by_label(label: &str) {
    if let Some(i) = THEMES
        .iter()
        .position(|t| t.label.eq_ignore_ascii_case(label))
    {
        IDX.store(i, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reset theme to Cyberpunk before each test to avoid cross-test interference.
    fn reset() {
        set_by_label("Cyberpunk");
    }

    #[test]
    fn test_current_default_is_cyberpunk() {
        reset();
        assert_eq!(current().label, "Cyberpunk");
    }

    #[test]
    fn test_current_label_matches_current() {
        reset();
        assert_eq!(current_label(), current().label);
    }

    #[test]
    fn test_cycle_advances_through_all_themes() {
        reset();
        // Starting at Cyberpunk (index 0), cycling should go: 1=Synthwave, 2=Matrix, 3=Amber CRT, 4=Ice
        assert_eq!(cycle(), "Synthwave");
        assert_eq!(cycle(), "Matrix");
        assert_eq!(cycle(), "Amber CRT");
        assert_eq!(cycle(), "Ice");
    }

    #[test]
    fn test_cycle_wraps_around() {
        reset();
        // Cycle through all 5 themes (4 cycles from index 0 → index 4)
        for _ in 0..4 {
            cycle();
        }
        assert_eq!(current().label, "Ice");
        // One more cycle should wrap back to Cyberpunk
        assert_eq!(cycle(), "Cyberpunk");
        assert_eq!(current().label, "Cyberpunk");
    }

    #[test]
    fn test_set_by_label_exact() {
        reset();
        set_by_label("Matrix");
        assert_eq!(current().label, "Matrix");
    }

    #[test]
    fn test_set_by_label_case_insensitive() {
        reset();
        set_by_label("MATRIX");
        assert_eq!(current().label, "Matrix");

        set_by_label("matrix");
        assert_eq!(current().label, "Matrix");

        set_by_label("MaTrIx");
        assert_eq!(current().label, "Matrix");
    }

    #[test]
    fn test_set_by_label_nonexistent_is_noop() {
        reset();
        set_by_label("nonexistent");
        // Should still be Cyberpunk
        assert_eq!(current().label, "Cyberpunk");
    }

    #[test]
    fn test_set_by_label_all_themes() {
        for theme in &THEMES {
            set_by_label(theme.label);
            assert_eq!(current().label, theme.label);
        }
        // Reset for other tests
        reset();
    }

    #[test]
    fn test_themes_count() {
        assert_eq!(THEMES.len(), 5);
    }

    #[test]
    fn test_theme_labels() {
        let labels: Vec<&str> = THEMES.iter().map(|t| t.label).collect();
        assert_eq!(
            labels,
            vec!["Cyberpunk", "Synthwave", "Matrix", "Amber CRT", "Ice"]
        );
    }
}
