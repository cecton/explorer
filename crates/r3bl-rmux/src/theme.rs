use r3bl_tui::{AnsiValue, RgbValue, TuiColor, TuiStyleAttribs, tui_style_attrib};
use rmux_sdk::{PaneAttributes, PaneColor};

pub fn map_color(color: &PaneColor) -> Option<TuiColor> {
    match color {
        PaneColor::Default | PaneColor::None | PaneColor::Terminal => None,
        PaneColor::Ansi { index } => Some(TuiColor::Ansi(AnsiValue { index: *index })),
        PaneColor::BrightAnsi { index } => Some(TuiColor::Ansi(AnsiValue { index: index + 8 })),
        PaneColor::Indexed { index } => Some(TuiColor::Ansi(AnsiValue { index: *index })),
        PaneColor::Rgb { red, green, blue } => Some(TuiColor::Rgb(RgbValue {
            red: *red,
            green: *green,
            blue: *blue,
        })),
        PaneColor::Encoded { value } => {
            let decoded = PaneColor::from_encoded(*value);
            if matches!(decoded, PaneColor::Encoded { .. }) {
                None
            } else {
                map_color(&decoded)
            }
        }
        _ => None,
    }
}

pub fn map_attributes(attrs: &PaneAttributes) -> TuiStyleAttribs {
    let mut a = TuiStyleAttribs::default();

    if attrs.contains(PaneAttributes::BOLD) {
        a.bold = Some(tui_style_attrib::Bold);
    }
    if attrs.contains(PaneAttributes::DIM) {
        a.dim = Some(tui_style_attrib::Dim);
    }
    if attrs.contains(PaneAttributes::UNDERLINE)
        || attrs.contains(PaneAttributes::DOUBLE_UNDERLINE)
        || attrs.contains(PaneAttributes::CURLY_UNDERLINE)
        || attrs.contains(PaneAttributes::DOTTED_UNDERLINE)
        || attrs.contains(PaneAttributes::DASHED_UNDERLINE)
    {
        a.underline = Some(tui_style_attrib::Underline);
    }
    if attrs.contains(PaneAttributes::BLINK) {
        a.blink = Some(tui_style_attrib::BlinkMode::Slow);
    }
    if attrs.contains(PaneAttributes::REVERSE) {
        a.reverse = Some(tui_style_attrib::Reverse);
    }
    if attrs.contains(PaneAttributes::HIDDEN) {
        a.hidden = Some(tui_style_attrib::Hidden);
    }
    if attrs.contains(PaneAttributes::ITALIC) {
        a.italic = Some(tui_style_attrib::Italic);
    }
    if attrs.contains(PaneAttributes::STRIKETHROUGH) {
        a.strikethrough = Some(tui_style_attrib::Strikethrough);
    }
    if attrs.contains(PaneAttributes::OVERLINE) {
        a.overline = Some(tui_style_attrib::Overline);
    }

    a
}

pub fn glyph_char(glyph: &rmux_sdk::PaneGlyph) -> char {
    if glyph.padding {
        ' '
    } else {
        glyph.text.chars().next().unwrap_or(' ')
    }
}
