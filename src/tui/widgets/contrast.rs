//! Colours that have to stay readable against a background they were not chosen for.
//!
//! A drag that marks text repaints it over the marker colour, and a symbol highlight
//! repaints a span over a palette colour: both were picked against the pane background,
//! so the foreground has to give way when the new background is close in brightness.

#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// Perceived brightness on a 0-255 scale (standard luma weights). Good enough for
/// picking readable terminal text; not meant to be WCAG-certified color science.
fn luminance(c: RgbValue) -> f32 {
    0.299 * c.red as f32 + 0.587 * c.green as f32 + 0.114 * c.blue as f32
}

/// Minimum acceptable luminance gap between text and its background, on the same
/// 0-255 scale as `luminance`. Chosen empirically against the symbol-highlight
/// palette (`SYMBOL_PALETTE`), several of whose colors sit close in perceived
/// brightness to common syntax-highlight foregrounds despite differing in hue.
const MIN_FG_BG_CONTRAST: f32 = 100.0;

/// Returns `fg` unchanged if it contrasts adequately against `bg`; otherwise swaps
/// to near-black or near-white, whichever contrasts better with `bg`. Applied to
/// every span so it transparently covers symbol-highlight and selection overlay
/// backgrounds as well as the normal pane background, without either call site
/// needing to know which case it is. A color of the terminal's palette or its default
/// has no brightness Explorer knows, so with one on either side `fg` is kept, as Helix
/// always keeps it.
pub(crate) fn ensure_readable_fg(
    fg: crate::tui::theme::Color,
    bg: crate::tui::theme::Color,
) -> crate::tui::theme::Color {
    use crate::tui::theme::Color;
    let (Some(fg_rgb), Some(bg_rgb)) = (fg.rgb(), bg.rgb()) else {
        return fg;
    };
    if (luminance(fg_rgb) - luminance(bg_rgb)).abs() >= MIN_FG_BG_CONTRAST {
        return fg;
    }
    if luminance(bg_rgb) > 127.0 {
        Color::from_u8(20, 20, 20)
    } else {
        Color::from_u8(235, 235, 235)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Color;

    #[test]
    fn keeps_fg_when_contrast_is_already_adequate() {
        // White text on a near-black background: plenty of contrast, unchanged.
        assert_eq!(
            ensure_readable_fg(Color::from_u8(230, 230, 230), Color::from_u8(10, 10, 10)),
            Color::from_u8(230, 230, 230)
        );
    }

    #[test]
    fn overrides_fg_when_luminance_nearly_matches() {
        // Yellow-ish syntax fg on a similarly bright symbol-highlight orange bg
        // (SYMBOL_PALETTE[2] = [255, 200, 100]) — nearly identical luminance.
        let fg = Color::from_u8(255, 255, 100);
        let bg = Color::from_u8(255, 200, 100);
        let result = ensure_readable_fg(fg, bg)
            .rgb()
            .expect("an RGB pair stays RGB");
        assert_ne!(Some(result), fg.rgb());
        let bg = bg.rgb().expect("an RGB background");
        assert!((luminance(result) - luminance(bg)).abs() >= MIN_FG_BG_CONTRAST);
    }

    #[test]
    fn overridden_fg_picks_dark_text_on_light_bg_and_light_text_on_dark_bg() {
        assert_eq!(
            ensure_readable_fg(Color::from_u8(255, 255, 100), Color::from_u8(255, 200, 100)),
            Color::from_u8(20, 20, 20)
        );
        assert_eq!(
            ensure_readable_fg(Color::from_u8(50, 50, 50), Color::from_u8(30, 30, 30)),
            Color::from_u8(235, 235, 235)
        );
    }

    /// The terminal's colors have no brightness Explorer knows: the foreground stays.
    #[test]
    fn keeps_fg_when_either_color_is_the_terminals() {
        let dark = Color::from_u8(30, 30, 30);
        assert_eq!(ensure_readable_fg(Color::Ansi(8), dark), Color::Ansi(8));
        assert_eq!(ensure_readable_fg(dark, Color::Default), dark);
    }
}
