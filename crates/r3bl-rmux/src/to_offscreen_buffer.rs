use crate::theme::{glyph_char, map_attributes, map_color};
use r3bl_tui::{ChUnit, ColIndex, OffscreenBuffer, PixelChar, Pos, RowIndex, Size, height, width};
use rmux_sdk::PaneSnapshot;

pub fn to_offscreen_buffer(snapshot: &PaneSnapshot) -> OffscreenBuffer {
    let size = Size {
        col_width: width(snapshot.cols),
        row_height: height(snapshot.rows),
    };
    let mut buf = OffscreenBuffer::new_empty(size);

    for (row, col, cell) in snapshot.visible_cells() {
        let row_idx = row as usize;
        let col_idx = col as usize;

        if col_idx >= snapshot.cols as usize || row_idx >= snapshot.rows as usize {
            continue;
        }

        if cell.is_padding() {
            buf.buffer[row_idx][col_idx] = PixelChar::Void;
        } else {
            let ch = glyph_char(&cell.glyph);
            let fg = map_color(&cell.foreground);
            let bg = map_color(&cell.background);
            let attribs = map_attributes(&cell.attributes);

            let style = if fg.is_some() || bg.is_some() || !attribs.is_none() {
                r3bl_tui::TuiStyle {
                    color_fg: fg,
                    color_bg: bg,
                    attribs,
                    ..Default::default()
                }
            } else {
                r3bl_tui::TuiStyle::default()
            };

            buf.buffer[row_idx][col_idx] = PixelChar::PlainText {
                display_char: ch,
                style,
            };
        }
    }

    buf.cursor_pos = Pos {
        row_index: RowIndex(ChUnit::new(snapshot.cursor.row)),
        col_index: ColIndex(ChUnit::new(snapshot.cursor.col)),
    };

    buf
}
