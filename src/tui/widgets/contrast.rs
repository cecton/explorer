//! Colours that have to stay readable against a background they were not chosen for.
//!
//! A drag that marks text repaints it over the marker colour, and a symbol highlight
//! repaints a span over a palette colour: both were picked against the pane background,
//! so the foreground has to give way when the new background is close in brightness.

#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// A drag that moves a highlight range rather than selecting text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragModifier {
    /// Adds the dragged range to the highlights.
    Shift,
    /// Removes the dragged range from the highlights.
    Ctrl,
}

/// The modifier a mouse event carries, when it carries exactly one of the two that mark.
pub(crate) fn drag_modifier_from_mouse(mouse: &MouseInput) -> Option<DragModifier> {
    let mask = mouse.maybe_modifier_keys?;
    let shift = mask.shift_key_state == KeyState::Pressed;
    let ctrl = mask.ctrl_key_state == KeyState::Pressed;
    let alt = mask.alt_key_state == KeyState::Pressed;
    if alt {
        return None;
    }
    match (shift, ctrl) {
        (true, false) => Some(DragModifier::Shift),
        (false, true) => Some(DragModifier::Ctrl),
        _ => None,
    }
}

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
/// needing to know which case it is.
pub(crate) fn ensure_readable_fg(fg: RgbValue, bg: RgbValue) -> RgbValue {
    if (luminance(fg) - luminance(bg)).abs() >= MIN_FG_BG_CONTRAST {
        return fg;
    }
    if luminance(bg) > 127.0 {
        RgbValue::from_u8(20, 20, 20)
    } else {
        RgbValue::from_u8(235, 235, 235)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_fg_when_contrast_is_already_adequate() {
        // White text on a near-black background: plenty of contrast, unchanged.
        assert_eq!(
            ensure_readable_fg(
                RgbValue::from_u8(230, 230, 230),
                RgbValue::from_u8(10, 10, 10),
            ),
            RgbValue::from_u8(230, 230, 230)
        );
    }

    #[test]
    fn overrides_fg_when_luminance_nearly_matches() {
        // Yellow-ish syntax fg on a similarly bright symbol-highlight orange bg
        // (SYMBOL_PALETTE[2] = [255, 200, 100]) — nearly identical luminance.
        let fg = RgbValue::from_u8(255, 255, 100);
        let bg = RgbValue::from_u8(255, 200, 100);
        let result = ensure_readable_fg(fg, bg);
        assert_ne!(result, fg);
        assert!((luminance(result) - luminance(bg)).abs() >= MIN_FG_BG_CONTRAST);
    }

    #[test]
    fn overridden_fg_picks_dark_text_on_light_bg_and_light_text_on_dark_bg() {
        assert_eq!(
            ensure_readable_fg(
                RgbValue::from_u8(255, 255, 100),
                RgbValue::from_u8(255, 200, 100),
            ),
            RgbValue::from_u8(20, 20, 20)
        );
        assert_eq!(
            ensure_readable_fg(RgbValue::from_u8(50, 50, 50), RgbValue::from_u8(30, 30, 30),),
            RgbValue::from_u8(235, 235, 235)
        );
    }

    #[test]
    fn a_drag_marks_with_exactly_one_modifier() {
        use r3bl_tui::KeyState;
        let with = |shift: KeyState, ctrl: KeyState, alt: KeyState| MouseInput {
            kind: r3bl_tui::MouseInputKind::MouseDrag(r3bl_tui::Button::Left),
            pos: r3bl_tui::VPPos::default(),
            maybe_modifier_keys: Some(ModifierKeysMask {
                shift_key_state: shift,
                ctrl_key_state: ctrl,
                alt_key_state: alt,
            }),
        };
        let (on, off) = (KeyState::Pressed, KeyState::NotPressed);
        assert_eq!(
            drag_modifier_from_mouse(&with(on, off, off)),
            Some(DragModifier::Shift)
        );
        assert_eq!(
            drag_modifier_from_mouse(&with(off, on, off)),
            Some(DragModifier::Ctrl)
        );
        assert_eq!(drag_modifier_from_mouse(&with(on, on, off)), None);
        assert_eq!(drag_modifier_from_mouse(&with(on, off, on)), None);
        assert_eq!(drag_modifier_from_mouse(&with(off, off, off)), None);
        assert_eq!(
            drag_modifier_from_mouse(&MouseInput {
                kind: r3bl_tui::MouseInputKind::MouseDrag(r3bl_tui::Button::Left),
                pos: r3bl_tui::VPPos::default(),
                maybe_modifier_keys: None,
            }),
            None
        );
    }
}
