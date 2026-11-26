/// Paints `text` with one op per run of one style, the styles `style_at` gives per char
/// index, instead of one op and one string per char.
pub(crate) fn paint_runs(
    ops: &mut r3bl_tui::RenderOpIRVec,
    text: &str,
    style_at: impl Fn(usize) -> r3bl_tui::TuiStyle,
) {
    let mut start = 0;
    let mut current: Option<r3bl_tui::TuiStyle> = None;
    for (index, (byte, _)) in text.char_indices().enumerate() {
        let style = style_at(index);
        if current != Some(style) {
            if let Some(done) = current {
                *ops += r3bl_tui::RenderOpCommon::ApplyColors(Some(done));
                *ops += r3bl_tui::RenderOpIR::PaintTextWithAttributes(
                    text[start..byte].into(),
                    Some(done),
                );
            }
            current = Some(style);
            start = byte;
        }
    }
    if let Some(done) = current {
        *ops += r3bl_tui::RenderOpCommon::ApplyColors(Some(done));
        *ops += r3bl_tui::RenderOpIR::PaintTextWithAttributes(text[start..].into(), Some(done));
    }
}

/// Whether `indices`, sorted, hold `index`: the style a highlight gives a char.
pub(crate) fn matched(indices: &[u32], index: usize) -> bool {
    indices.binary_search(&(index as u32)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_of_one_style_are_painted_whole() {
        #[allow(clippy::wildcard_imports)] // the style macros: see AGENTS.md
        use r3bl_tui::*;
        let mut ops = RenderOpIRVec::new();
        let bold = new_style!(bold);
        let plain = new_style!();
        paint_runs(&mut ops, "hello world", |index| {
            if index < 5 { bold } else { plain }
        });
        let painted: Vec<String> = ops
            .iter()
            .filter_map(|op| match op {
                RenderOpIR::PaintTextWithAttributes(text, _) => Some(text.as_str().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(painted, ["hello", " world"]);
    }

    /// The compositor takes a color the style leaves `None` (the terminal's default) from
    /// the pen, so each run sets the pen to its own style before it is painted.
    #[test]
    fn each_run_sets_the_pen_to_its_style() {
        #[allow(clippy::wildcard_imports)] // the style macros: see AGENTS.md
        use r3bl_tui::*;
        let mut ops = RenderOpIRVec::new();
        let bold = new_style!(bold);
        let plain = new_style!();
        paint_runs(&mut ops, "hello world", |index| {
            if index < 5 { bold } else { plain }
        });
        let mut pen = None;
        for op in ops.iter() {
            match op {
                RenderOpIR::Common(RenderOpCommon::ApplyColors(style)) => pen = *style,
                RenderOpIR::PaintTextWithAttributes(_, style) => assert_eq!(pen, *style),
                _ => {}
            }
        }
    }

    #[test]
    fn matched_finds_a_char_the_indices_hold() {
        assert!(matched(&[0, 1, 5], 5));
        assert!(!matched(&[0, 1, 5], 2));
        assert!(!matched(&[], 0));
    }
}
