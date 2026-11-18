#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::collections::HashMap;

include!("../../themes/themes.rs");

/// A color as the terminal is told to paint it. Helix themes name colors three ways, and
/// each reaches the screen differently: a hex value is painted as is, one of the 16 color
/// names (`red`, `light-gray`, …) is the terminal's own palette entry, and a background or
/// a text the theme leaves unset keeps the terminal's default color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    Rgb(RgbValue),
    /// An index of the terminal's palette, 0 to 15.
    Ansi(u8),
    /// The terminal's default foreground or background.
    #[default]
    Default,
}

impl Color {
    pub const fn from_u8(red: u8, green: u8, blue: u8) -> Self {
        Self::Rgb(RgbValue { red, green, blue })
    }

    /// The color r3bl paints, `None` for the terminal's default. Painted through a style
    /// set with `ApplyColors`, a `None` side clears the pen, so the terminal's own color
    /// shows; `SetBgColor` cannot say that, so theme colors never go through it.
    pub fn tui(self) -> Option<TuiColor> {
        match self {
            Self::Rgb(rgb) => Some(TuiColor::Rgb(rgb)),
            Self::Ansi(index) => Some(TuiColor::Ansi(AnsiValue::from(index))),
            Self::Default => None,
        }
    }

    /// The RGB value, when the theme gave one: the terminal's palette and default colors
    /// are unknown to Explorer, so color arithmetic runs on this alone.
    pub fn rgb(self) -> Option<RgbValue> {
        match self {
            Self::Rgb(rgb) => Some(rgb),
            Self::Ansi(_) | Self::Default => None,
        }
    }

    /// This color as the foreground over `bg`.
    pub fn on(self, bg: Color) -> TuiStyle {
        TuiStyle {
            color_fg: self.tui(),
            color_bg: bg.tui(),
            ..TuiStyle::default()
        }
    }

    /// This color as a bold foreground over `bg`.
    pub fn on_bold(self, bg: Color) -> TuiStyle {
        let mut style = self.on(bg);
        style.attribs.bold = Some(tui_style_attrib::Bold);
        style
    }
}

/// What a theme leaves unset falls back to these. The background and the text have none:
/// like Helix, Explorer leaves them to the terminal.
const DEFAULT_SELECTION_BG: Color = Color::from_u8(50, 50, 90);
const DEFAULT_SELECTION_FG: Color = Color::from_u8(255, 255, 255);
const DEFAULT_STATUSLINE_BG: Color = Color::from_u8(30, 30, 50);
const DEFAULT_STATUSLINE_FG: Color = Color::from_u8(180, 180, 220);
const DEFAULT_ERROR_FG: Color = Color::from_u8(220, 80, 80);
const DEFAULT_WARNING_FG: Color = Color::from_u8(220, 170, 80);
const DEFAULT_MATCH_FG: Color = Color::from_u8(255, 200, 60);
const DEFAULT_SCROLL_THUMB: Color = Color::from_u8(120, 120, 160);

/// The colors of Explorer's own surfaces, resolved once when the theme is built so every
/// pane paints a role the same way. Each role names the theme keys it is read from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ui {
    /// `ui.background` bg.
    pub background: Color,
    /// `ui.text` fg.
    pub text: Color,
    /// Secondary text: the `comment` scope, else `text`.
    pub dim: Color,
    /// `ui.selection` bg.
    pub selection: Color,
    /// `ui.selection` fg: a line number on a highlighted line.
    pub selection_fg: Color,
    /// `ui.linenr` fg, else `dim`.
    pub line_number: Color,
    /// `ui.linenr` bg, else `background`.
    pub line_number_bg: Color,
    /// `ui.linenr.selected` fg, else `text`.
    pub line_number_selected: Color,
    /// `ui.cursor` bg, then `ui.text.focus` fg, else `text`.
    pub cursor: Color,
    /// `ui.cursor.match` fg: fuzzy matches and favorites.
    pub matched: Color,
    /// `error` fg.
    pub error: Color,
    /// `warning` fg.
    pub warning: Color,
    /// `ui.statusline` fg.
    pub statusline_fg: Color,
    /// `ui.statusline` bg.
    pub statusline_bg: Color,
    /// The Discussion's prompt block: `ui.cursorline` bg, then `ui.cursorline.primary`
    /// bg, else `statusline_bg`.
    pub prompt: Color,
    /// The Discussion's input: `ui.help` bg, else `background`.
    pub input: Color,
    /// `ui.virtual.ruler` bg: a markdown code block, `None` paints none.
    pub ruler: Option<Color>,
    /// `ui.menu.scroll` bg, then `ui.virtual.ruler` bg, else `statusline_bg`.
    pub scroll_track: Color,
    /// `ui.menu.scroll` fg, then the bg of `ui.selection`, `ui.cursorline.primary` and
    /// `ui.cursorline`, else `selection`; `cursor` when that is the track color, then a
    /// constant when that is still the track color.
    pub scroll_thumb: Color,
}

#[derive(Clone, PartialEq, Debug)]
pub struct HelixTheme {
    name: String,
    styles: HashMap<String, Style>,
    token_colors: [Option<Color>; crate::highlight::TokenKind::COUNT],
    pub ui: Ui,
}

impl Default for HelixTheme {
    /// A theme that styles nothing, with its `ui` resolved like any other, as the default
    /// `AppState` (derived `Default`) needs: a derived default would leave every role at
    /// the terminal's default. The inherent `HelixTheme::default()` is catppuccin_mocha.
    fn default() -> Self {
        Self::build("", "")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
}

struct RawTheme {
    palette: HashMap<String, RgbValue>,
    styles: Vec<(String, serde_json::Value)>,
    inherits: Option<String>,
}

impl HelixTheme {
    /// The theme `name` names, built once and handed out as a copy from then on: parsing
    /// a theme and the parents it inherits takes about 270 µs, and the Theme Picker asks
    /// for one on every arrow press as it previews.
    pub fn from_name(name: &str) -> Option<Self> {
        use std::sync::{Mutex, OnceLock};
        static BUILT: OnceLock<Mutex<HashMap<String, HelixTheme>>> = OnceLock::new();
        let mut built = BUILT
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .expect("theme cache");
        if let Some(theme) = built.get(name) {
            return Some(theme.clone());
        }
        let (_, toml_str) = THEMES.iter().find(|(n, _)| *n == name)?;
        let theme = Self::build(name, toml_str);
        built.insert(name.to_string(), theme.clone());
        Some(theme)
    }

    pub fn default() -> Self {
        let default_name = "catppuccin_mocha";
        Self::from_name(default_name).expect("default theme not found")
    }

    pub fn theme_names() -> impl Iterator<Item = &'static str> {
        THEMES.iter().map(|(n, _)| *n)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn build(name: &str, toml_str: &str) -> Self {
        let chain = collect_chain(name, toml_str);

        let mut styles: HashMap<String, Style> = HashMap::new();
        let mut palette: HashMap<&str, RgbValue> = HashMap::new();

        for raw in chain.iter().rev() {
            for (k, &v) in &raw.palette {
                palette.insert(k.as_str(), v);
            }
        }

        for raw in chain.iter().rev() {
            for (key, val) in &raw.styles {
                if let Some(style) = resolve_style(val, &palette) {
                    styles.insert(key.clone(), style);
                }
            }
        }

        let mut theme = Self {
            name: name.to_string(),
            styles,
            token_colors: [None; crate::highlight::TokenKind::COUNT],
            ui: Ui::default(),
        };
        for (index, kind) in crate::highlight::TokenKind::ALL.into_iter().enumerate() {
            theme.token_colors[index] =
                kind.scopes().iter().find_map(|scope| theme.scope_fg(scope));
        }
        theme.ui = theme.resolve_ui();
        theme
    }

    /// Every role of `Ui` from the styles, each through its own fallback chain.
    fn resolve_ui(&self) -> Ui {
        let fg = |key: &str| self.styles.get(key).and_then(|style| style.fg);
        let bg = |key: &str| self.styles.get(key).and_then(|style| style.bg);
        let background = bg("ui.background").unwrap_or(Color::Default);
        let text = fg("ui.text").unwrap_or(Color::Default);
        let dim = self.scope_fg("comment").unwrap_or(text);
        let selection = bg("ui.selection").unwrap_or(DEFAULT_SELECTION_BG);
        let cursor = bg("ui.cursor")
            .or_else(|| fg("ui.text.focus"))
            .unwrap_or(text);
        let statusline_bg = bg("ui.statusline").unwrap_or(DEFAULT_STATUSLINE_BG);
        let ruler = bg("ui.virtual.ruler");
        let scroll_track = bg("ui.menu.scroll").or(ruler).unwrap_or(statusline_bg);
        let mut scroll_thumb = fg("ui.menu.scroll")
            .or_else(|| bg("ui.selection"))
            .or_else(|| bg("ui.cursorline.primary"))
            .or_else(|| bg("ui.cursorline"))
            .unwrap_or(selection);
        if scroll_thumb == scroll_track {
            scroll_thumb = cursor;
        }
        if scroll_thumb == scroll_track {
            scroll_thumb = DEFAULT_SCROLL_THUMB;
        }
        Ui {
            background,
            text,
            dim,
            selection,
            selection_fg: fg("ui.selection").unwrap_or(DEFAULT_SELECTION_FG),
            line_number: fg("ui.linenr").unwrap_or(dim),
            line_number_bg: bg("ui.linenr").unwrap_or(background),
            line_number_selected: fg("ui.linenr.selected").unwrap_or(text),
            cursor,
            matched: fg("ui.cursor.match").unwrap_or(DEFAULT_MATCH_FG),
            error: fg("error").unwrap_or(DEFAULT_ERROR_FG),
            warning: fg("warning").unwrap_or(DEFAULT_WARNING_FG),
            statusline_fg: fg("ui.statusline").unwrap_or(DEFAULT_STATUSLINE_FG),
            statusline_bg,
            prompt: bg("ui.cursorline")
                .or_else(|| bg("ui.cursorline.primary"))
                .unwrap_or(statusline_bg),
            input: bg("ui.help").unwrap_or(background),
            ruler,
            scroll_track,
            scroll_thumb,
        }
    }

    /// The color a scope resolves to, falling back to `ui.text` when the theme styles
    /// none of its dotted parents.
    pub fn color_for_scope(&self, scope: &str) -> Color {
        self.scope_fg(scope).unwrap_or(self.ui.text)
    }

    /// The color the theme gives a scope or its nearest styled dotted parent, `None`
    /// when it styles none of them.
    fn scope_fg(&self, scope: &str) -> Option<Color> {
        let mut s = scope;
        loop {
            if let Some(fg) = self.styles.get(s).and_then(|style| style.fg) {
                return Some(fg);
            }
            s = &s[..s.rfind('.')?];
        }
    }

    /// The color the theme gives exactly `scope`, without walking its dotted parents:
    /// for markdown styles that only take a color the theme meant for them.
    pub fn exact_fg(&self, scope: &str) -> Option<Color> {
        self.styles.get(scope).and_then(|style| style.fg)
    }

    /// The color of a semantic token kind, `None` when the theme styles none of its
    /// scopes.
    pub fn token_color(&self, kind: crate::highlight::TokenKind) -> Option<Color> {
        self.token_colors[kind as usize]
    }
}

fn collect_chain(name: &str, toml_str: &str) -> Vec<RawTheme> {
    let mut chain = Vec::new();
    let mut current_name: Option<&str> = Some(name);
    let mut current_toml: Option<&str> = Some(toml_str);
    let mut visited = std::collections::HashSet::new();

    while let Some(n) = current_name {
        if !visited.insert(n) {
            break;
        }
        let Some(t) = current_toml else { break };
        let raw = parse_theme(t);
        let inherits = raw.inherits.clone();
        chain.push(raw);
        match inherits.as_deref() {
            Some(parent) => {
                let found = THEMES.iter().find(|(n, _)| *n == parent);
                match found {
                    Some((pn, pt)) => {
                        current_name = Some(pn);
                        current_toml = Some(pt);
                    }
                    None => break,
                }
            }
            None => break,
        }
    }

    chain
}

fn parse_theme(toml_str: &str) -> RawTheme {
    let root: serde_json::Value = match toml::from_str(toml_str) {
        Ok(v) => v,
        Err(_) => {
            return RawTheme {
                palette: HashMap::new(),
                styles: Vec::new(),
                inherits: None,
            };
        }
    };
    let table = match root.as_object() {
        Some(t) => t,
        None => {
            return RawTheme {
                palette: HashMap::new(),
                styles: Vec::new(),
                inherits: None,
            };
        }
    };

    let mut palette: HashMap<String, RgbValue> = HashMap::new();
    if let Some(palette_table) = table.get("palette").and_then(|v| v.as_object()) {
        for (name, val) in palette_table {
            if let Some(color) = val.as_str().and_then(parse_hex_color) {
                palette.insert(name.to_string(), color);
            }
        }
    }

    let inherits = table
        .get("inherits")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut styles: Vec<(String, serde_json::Value)> = Vec::new();
    for (key, val) in table {
        if key == "palette" || key == "inherits" {
            continue;
        }
        styles.push((key.clone(), val.clone()));
    }

    RawTheme {
        palette,
        styles,
        inherits,
    }
}

/// A style of the theme: a bare string is a foreground. A style whose `modifiers` hold
/// `reversed` has its colors swapped here, as the terminal would swap them, so a role read
/// from it (`ui.selection` as `{ fg = "gray", modifiers = ["reversed"] }`) gets the color
/// the user sees there.
fn resolve_style(val: &serde_json::Value, palette: &HashMap<&str, RgbValue>) -> Option<Style> {
    match val {
        serde_json::Value::String(s) => Some(Style {
            fg: Some(resolve_color(s, palette)?),
            bg: None,
        }),
        serde_json::Value::Object(obj) => {
            let color = |key: &str| {
                obj.get(key)
                    .and_then(|v| v.as_str())
                    .and_then(|s| resolve_color(s, palette))
            };
            let (fg, bg) = (color("fg"), color("bg"));
            let reversed = obj
                .get("modifiers")
                .and_then(|v| v.as_array())
                .is_some_and(|modifiers| modifiers.iter().any(|m| m.as_str() == Some("reversed")));
            Some(if reversed {
                Style { fg: bg, bg: fg }
            } else {
                Style { fg, bg }
            })
        }
        _ => None,
    }
}

/// A color as Helix reads it: the theme's palette first, then a hex value, then one of the
/// 16 names Helix gives the terminal's palette, numbered as its crossterm backend numbers
/// them (`gray` is the bright black 8, `light-gray` the plain white 7).
fn resolve_color(s: &str, palette: &HashMap<&str, RgbValue>) -> Option<Color> {
    if let Some(&rgb) = palette.get(s) {
        return Some(Color::Rgb(rgb));
    }
    if let Some(rgb) = parse_hex_color(s) {
        return Some(Color::Rgb(rgb));
    }
    let index = match s {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "light-gray" => 7,
        "gray" => 8,
        "light-red" => 9,
        "light-green" => 10,
        "light-yellow" => 11,
        "light-blue" => 12,
        "light-magenta" => 13,
        "light-cyan" => 14,
        "white" => 15,
        _ => return None,
    };
    Some(Color::Ansi(index))
}

fn parse_hex_color(s: &str) -> Option<RgbValue> {
    let s = s.strip_prefix('#').unwrap_or(s);
    let expanded = match s.len() {
        6 => s.to_string(),
        3 => s.chars().flat_map(|c| [c, c]).collect::<String>(),
        _ => return None,
    };
    RgbValue::try_from_hex_color(&format!("#{expanded}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(name: &str) -> HelixTheme {
        HelixTheme::from_name(name).expect("bundled theme")
    }

    /// A theme keeps what it was built with, so the copy the cache hands out is the same
    /// theme and an unknown name is still a miss.
    #[test]
    fn a_theme_is_built_once_and_handed_out_as_a_copy() {
        let first = theme("adwaita-dark");
        let second = theme("adwaita-dark");
        assert_eq!(first, second);
        assert_eq!(second.name(), "adwaita-dark");
        assert!(HelixTheme::from_name("no such theme").is_none());
    }

    #[test]
    fn a_scope_falls_back_to_its_styled_parent() {
        let theme = theme("catppuccin_mocha");
        assert_eq!(
            theme.scope_fg("keyword.control.return"),
            theme.scope_fg("keyword")
        );
        assert_eq!(
            Some(theme.color_for_scope("keyword.control.return")),
            theme.scope_fg("keyword")
        );
    }

    #[test]
    fn a_lsp_token_takes_the_first_scope_the_theme_styles() {
        use crate::highlight::TokenKind;

        let theme = theme("catppuccin_mocha");
        let keyword = theme.scope_fg("keyword").expect("keyword is styled");
        assert_eq!(
            theme.token_color(TokenKind::from_name("keyword")),
            Some(keyword)
        );
        assert_eq!(
            theme.token_color(TokenKind::from_name("comment")),
            theme.scope_fg("comment")
        );
        assert_eq!(
            theme.token_color(TokenKind::from_name("parameter")),
            theme.scope_fg("variable.parameter")
        );
        assert_eq!(
            theme.token_color(TokenKind::from_name("nonexistentToken")),
            None
        );
    }

    /// Themes that style only one member of a family keep their color for the whole
    /// family: `jetbrains_dark` has `type.builtin` and no `type`, `eiffel` has
    /// `constant.numeric` and no `constant`.
    #[test]
    fn a_token_falls_back_to_the_member_of_its_family_a_theme_styles() {
        use crate::highlight::TokenKind;

        let jetbrains = theme("jetbrains_dark");
        assert_eq!(jetbrains.scope_fg("type"), None);
        assert_eq!(
            jetbrains.token_color(TokenKind::from_name("struct")),
            jetbrains.scope_fg("type.builtin")
        );
        let eiffel = theme("eiffel");
        assert_eq!(eiffel.scope_fg("constant"), None);
        assert_eq!(
            eiffel.token_color(TokenKind::from_name("static")),
            eiffel.scope_fg("constant.builtin")
        );
    }

    /// The table `build` precomputes is the scope walk it replaced: the first scope the
    /// theme styles, and `None` — not `ui.text` — for a kind it styles nowhere.
    #[test]
    fn the_token_color_table_agrees_with_the_scope_walk() {
        use crate::highlight::TokenKind;

        let theme = theme("catppuccin_mocha");
        for kind in TokenKind::ALL {
            let expected = kind.scopes().iter().find_map(|scope| theme.scope_fg(scope));
            assert_eq!(theme.token_color(kind), expected, "{kind:?}");
            if expected.is_none() {
                assert_ne!(
                    theme.token_color(kind),
                    Some(theme.ui.text),
                    "{kind:?} must not fall back to ui.text",
                );
            }
        }
        assert_eq!(theme.token_color(TokenKind::Other), None);
        assert_ne!(theme.ui.text, Color::Default);
    }

    #[test]
    fn global_actions_resolve_an_unstyled_scope_to_ui_text() {
        let theme = theme("catppuccin_mocha");
        assert_eq!(theme.color_for_scope("no.such.scope"), theme.ui.text);
        assert_eq!(theme.scope_fg("no.such.scope"), None);
    }

    /// The 16-color themes name the terminal's palette, so their colors are its entries,
    /// numbered as Helix numbers them, and a reversed selection paints its fg behind the
    /// text.
    #[test]
    fn a_color_name_is_the_terminal_palette_entry() {
        use crate::highlight::TokenKind;

        let theme = theme("base16_terminal");
        assert_eq!(theme.scope_fg("keyword"), Some(Color::Ansi(13)));
        assert_eq!(
            theme.token_color(TokenKind::from_name("keyword")),
            Some(Color::Ansi(13))
        );
        assert_eq!(theme.ui.selection, Color::Ansi(8));
        assert_eq!(theme.ui.line_number, Color::Ansi(7));
    }

    /// Where a theme sets no background, or an empty one, the terminal's own shows, as in
    /// Helix; so does its own text color where the theme sets none.
    #[test]
    fn an_unset_background_or_text_is_the_terminal_default() {
        for name in ["term16_dark", "term16_light", "base16_transparent"] {
            let theme = theme(name);
            assert_eq!(theme.ui.background, Color::Default, "{name}");
            assert_eq!(theme.ui.background.tui(), None, "{name}");
        }
        assert_eq!(theme("term16_dark").ui.text, Color::Default);
    }

    /// A theme's palette entry wins over the terminal color of the same name.
    #[test]
    fn a_palette_entry_overrides_a_color_name() {
        let theme = HelixTheme::build(
            "test",
            "keyword = \"red\"\nstring = \"green\"\n[palette]\nred = \"#ff0000\"\n",
        );
        assert_eq!(theme.scope_fg("keyword"), Some(Color::from_u8(255, 0, 0)));
        assert_eq!(theme.scope_fg("string"), Some(Color::Ansi(2)));
    }

    /// The roles fall back along their chains to Explorer's constants when the theme
    /// styles nothing, and to the terminal's default for the background and the text.
    #[test]
    fn an_unstyled_theme_resolves_every_role() {
        let theme = HelixTheme::build("test", "");
        assert_eq!(theme.ui.background, Color::Default);
        assert_eq!(theme.ui.text, Color::Default);
        assert_eq!(theme.ui.dim, Color::Default);
        assert_eq!(theme.ui.selection, DEFAULT_SELECTION_BG);
        assert_eq!(theme.ui.prompt, DEFAULT_STATUSLINE_BG);
        assert_eq!(theme.ui.scroll_track, DEFAULT_STATUSLINE_BG);
        assert_ne!(theme.ui.scroll_thumb, theme.ui.scroll_track);
        assert_eq!(theme.ui.ruler, None);
        assert_eq!(<HelixTheme as Default>::default().ui, theme.ui);
    }
}
