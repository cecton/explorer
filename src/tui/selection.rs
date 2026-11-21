//! The one text selection of the application: which window it is in and the points it
//! spans, shared by the file preview, the terminal and the Discussion.

#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// One of the texts a Discussion window shows, in the order it shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DocPart {
    Prompt,
    WorkLog,
    Thinking,
    Reply,
}

/// A point in a window's content used as a text-selection anchor or endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelPoint {
    /// Point inside a file preview.
    Preview {
        /// Zero-based line index in the preview content.
        line_idx: usize,
        /// Byte offset within that line.
        byte_offset: usize,
    },
    /// Point inside a terminal pane viewport.
    Terminal {
        /// Zero-based row within the visible terminal viewport.
        viewport_row: usize,
        /// Zero-based column within that row.
        col: usize,
    },
    /// Point inside a Discussion window.
    Discussion {
        /// Text the point lies in.
        part: DocPart,
        /// Byte offset within that text.
        offset: usize,
    },
}

/// Active text selection within a single window.
#[derive(Clone, Debug)]
pub struct TextSelection {
    /// Window that owns this selection.
    pub window: crate::tui::pane_manager::Window,
    /// Selection start point.
    pub start: SelPoint,
    /// Selection end point.
    pub end: SelPoint,
    /// Point where the current click/drag sequence began.
    pub click_anchor: Option<SelPoint>,
    /// Word boundary pair detected from a single click.
    pub click_word: Option<(SelPoint, SelPoint)>,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_marks_with_exactly_one_modifier() {
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
