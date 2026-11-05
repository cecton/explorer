use crate::tui::RgbValue;
use serde_json::Value;
use std::collections::HashMap;

include!("../../themes/themes.rs");

#[derive(Clone, PartialEq, Debug, Default)]
pub struct HelixTheme {
    name: String,
    styles: HashMap<String, Style>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
    pub fg: Option<RgbValue>,
    pub bg: Option<RgbValue>,
}

struct RawTheme {
    palette: HashMap<String, RgbValue>,
    styles: Vec<(String, Value)>,
    inherits: Option<String>,
}

impl HelixTheme {
    pub fn from_name(name: &str) -> Option<Self> {
        let (_, toml_str) = THEMES.iter().find(|(n, _)| *n == name)?;
        Some(Self::build(name, toml_str))
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

        Self {
            name: name.to_string(),
            styles,
        }
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

    /// The color of a semantic token: that of the first of its scopes the theme styles,
    /// so a theme that sets only `keyword.control` still colors `keyword` tokens.
    pub fn color_for_lsp_token(&self, token_type: &str) -> Option<RgbValue> {
        lsp_token_scopes(token_type)
            .iter()
            .find_map(|scope| self.scope_fg(scope))
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
    let root: Value = match toml::from_str(toml_str) {
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

    let mut styles: Vec<(String, Value)> = Vec::new();
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

fn resolve_style(val: &Value, palette: &HashMap<&str, RgbValue>) -> Option<Style> {
    match val {
        Value::String(s) => {
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
        Value::Object(obj) => {
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

/// The theme scopes of a semantic token type, most specific first: the first one the
/// theme styles gives the token its color. Later entries are fallbacks for the bundled
/// themes that style only one member of a family (`type.builtin` without `type`,
/// `constant.numeric` without `constant`, …).
fn lsp_token_scopes(token_type: &str) -> &'static [&'static str] {
    match token_type {
        "keyword" => &["keyword", "keyword.control"],
        "modifier" => &["keyword.storage.modifier", "keyword"],
        "selfKeyword" | "boolean" => &["keyword", "variable.builtin"],
        "comment" => &["comment"],
        "string" | "character" | "escapeSequence" => &["string", "comment"],
        "number" => &["constant.numeric", "constant"],
        "const" | "static" => &["constant", "constant.builtin", "constant.numeric"],
        "namespace" => &["namespace", "type", "type.builtin"],
        "type" | "class" | "struct" | "enum" | "interface" | "builtinType" | "typeAlias"
        | "typeParameter" | "constParameter" | "generic" | "toolModule" => {
            &["type", "type.builtin"]
        }
        "function" | "method" => &["function", "function.method", "function.builtin"],
        "macro" | "attributeBracket" | "builtinAttribute" | "decorator" => &[
            "function.macro",
            "function.special",
            "attribute",
            "function",
        ],
        "variable" => &["variable"],
        "parameter" => &[
            "variable.parameter",
            "variable",
            "variable.other.member",
            "variable.builtin",
        ],
        "property" | "enumMember" => &["variable.other.member", "variable"],
        "operator" => &["operator"],
        "lifetime" => &["label", "operator"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(name: &str) -> HelixTheme {
        HelixTheme::from_name(name).expect("bundled theme")
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
        let theme = theme("catppuccin_mocha");
        let keyword = theme.scope_fg("keyword").expect("keyword is styled");
        assert_eq!(theme.color_for_lsp_token("keyword"), Some(keyword));
        assert_eq!(
            theme.color_for_lsp_token("comment"),
            theme.scope_fg("comment")
        );
        assert_eq!(
            theme.color_for_lsp_token("parameter"),
            theme.scope_fg("variable.parameter")
        );
        assert_eq!(theme.color_for_lsp_token("nonexistentToken"), None);
    }

    /// Themes that style only one member of a family keep their color for the whole
    /// family: `jetbrains_dark` has `type.builtin` and no `type`, `eiffel` has
    /// `constant.numeric` and no `constant`.
    #[test]
    fn a_token_falls_back_to_the_member_of_its_family_a_theme_styles() {
        let jetbrains = theme("jetbrains_dark");
        assert_eq!(jetbrains.scope_fg("type"), None);
        assert_eq!(
            jetbrains.color_for_lsp_token("struct"),
            jetbrains.scope_fg("type.builtin")
        );
        let eiffel = theme("eiffel");
        assert_eq!(eiffel.scope_fg("constant"), None);
        assert_eq!(
            eiffel.color_for_lsp_token("static"),
            eiffel.scope_fg("constant.builtin")
        );
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
