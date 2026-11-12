#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::collections::HashMap;

include!("../../themes/themes.rs");

#[derive(Clone, PartialEq, Debug, Default)]
pub struct HelixTheme {
    name: String,
    styles: HashMap<String, Style>,
    token_colors: [Option<RgbValue>; crate::highlight::TokenKind::COUNT],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
    pub fg: Option<RgbValue>,
    pub bg: Option<RgbValue>,
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
        };
        for (index, kind) in crate::highlight::TokenKind::ALL.into_iter().enumerate() {
            theme.token_colors[index] =
                kind.scopes().iter().find_map(|scope| theme.scope_fg(scope));
        }
        theme
    }

    /// The color a scope resolves to, falling back to `ui.text` when the theme styles
    /// none of its dotted parents.
    pub fn color_for_scope(&self, scope: &str) -> Option<RgbValue> {
        self.scope_fg(scope)
            .or_else(|| self.styles.get("ui.text").and_then(|s| s.fg))
    }

    /// The color the theme gives a scope or its nearest styled dotted parent, `None`
    /// when it styles none of them.
    fn scope_fg(&self, scope: &str) -> Option<RgbValue> {
        let mut s = scope;
        loop {
            if let Some(fg) = self.styles.get(s).and_then(|style| style.fg) {
                return Some(fg);
            }
            s = &s[..s.rfind('.')?];
        }
    }

    /// The color of a semantic token kind, `None` when the theme styles none of its
    /// scopes.
    pub fn token_color(&self, kind: crate::highlight::TokenKind) -> Option<RgbValue> {
        self.token_colors[kind as usize]
    }

    pub fn ui_fg(&self, key: &str) -> Option<RgbValue> {
        self.styles.get(key).and_then(|s| s.fg)
    }

    pub fn ui_bg(&self, key: &str) -> Option<RgbValue> {
        self.styles.get(key).and_then(|s| s.bg)
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

fn resolve_style(val: &serde_json::Value, palette: &HashMap<&str, RgbValue>) -> Option<Style> {
    match val {
        serde_json::Value::String(s) => {
            if let Some(rgb) = parse_hex_color(s) {
                return Some(Style {
                    fg: Some(rgb),
                    bg: None,
                });
            }
            palette.get(s.as_str()).map(|rgb| Style {
                fg: Some(*rgb),
                bg: None,
            })
        }
        serde_json::Value::Object(obj) => {
            let fg = obj
                .get("fg")
                .and_then(|v| v.as_str())
                .and_then(|s| parse_hex_color(s).or_else(|| palette.get(s).copied()));
            let bg = obj
                .get("bg")
                .and_then(|v| v.as_str())
                .and_then(|s| parse_hex_color(s).or_else(|| palette.get(s).copied()));
            Some(Style { fg, bg })
        }
        _ => None,
    }
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
            theme.color_for_scope("keyword.control.return"),
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
                    theme.ui_fg("ui.text"),
                    "{kind:?} must not fall back to ui.text",
                );
            }
        }
        assert_eq!(theme.token_color(TokenKind::Other), None);
        assert!(theme.ui_fg("ui.text").is_some());
    }

    #[test]
    fn global_actions_resolve_an_unstyled_scope_to_ui_text() {
        let theme = theme("catppuccin_mocha");
        assert_eq!(
            theme.color_for_scope("no.such.scope"),
            theme.ui_fg("ui.text")
        );
        assert_eq!(theme.scope_fg("no.such.scope"), None);
    }
}
