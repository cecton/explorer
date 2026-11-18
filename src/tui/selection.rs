//! The one text selection of the application: which window it is in and the points it
//! spans, shared by the file preview, the terminal and the Discussion.

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
    /// Whether the selection is still being dragged.
    pub active: bool,
}
