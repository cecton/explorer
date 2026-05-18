use serde_json::Value;
use std::collections::HashMap;

include!("../../themes/themes.rs");

#[derive(Clone)]
pub struct HelixTheme {
    styles: HashMap<String, Style>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Style {
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
}

struct RawTheme {
    palette: HashMap<String, [u8; 3]>,
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

    fn build(name: &str, toml_str: &str) -> Self {
        let chain = collect_chain(name, toml_str);

        let mut styles: HashMap<String, Style> = HashMap::new();
        let mut palette: HashMap<&str, [u8; 3]> = HashMap::new();

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

        Self { styles }
    }

    pub fn color_for_scope(&self, scope: &str) -> Option<[u8; 3]> {
        let mut s = scope;
        loop {
            if let Some(style) = self.styles.get(s)
                && let Some(fg) = style.fg
            {
                return Some(fg);
            }
            if let Some(dot) = s.rfind('.') {
                s = &s[..dot];
            } else {
                break;
            }
        }
        self.styles.get("ui.text").and_then(|s| s.fg)
    }

    pub fn color_for_lsp_token(&self, token_type: &str) -> Option<[u8; 3]> {
        for scope in lsp_token_scopes(token_type) {
            if let Some(color) = self.color_for_scope(scope) {
                return Some(color);
            }
        }
        None
    }

    pub fn ui_fg(&self, key: &str) -> Option<[u8; 3]> {
        self.styles.get(key).and_then(|s| s.fg)
    }

    pub fn ui_bg(&self, key: &str) -> Option<[u8; 3]> {
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

    let mut palette: HashMap<String, [u8; 3]> = HashMap::new();
    if let Some(palette_table) = table.get("palette").and_then(|v| v.as_object()) {
        for (name, val) in palette_table {
            if let Some(hex) = val.as_str().and_then(parse_hex_color) {
                palette.insert(name.to_string(), hex);
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

fn resolve_style(val: &Value, palette: &HashMap<&str, [u8; 3]>) -> Option<Style> {
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

fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    let s = s.strip_prefix('#').unwrap_or(s);
    match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some([r, g, b])
        }
        3 => {
            let r = u8::from_str_radix(&s[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&s[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&s[2..3], 16).ok()? * 17;
            Some([r, g, b])
        }
        _ => None,
    }
}

fn lsp_token_scopes(token_type: &str) -> &[&str] {
    match token_type {
        "keyword" | "modifier" | "selfKeyword" | "boolean" => {
            &["keyword", "keyword.control", "keyword.storage.modifier"]
        }
        "string" | "comment" | "character" | "escapeSequence" => &["string", "comment"],
        "number" | "const" | "static" => &["constant.numeric", "constant"],
        "type" | "class" | "struct" | "enum" | "interface" | "namespace" | "builtinType"
        | "typeAlias" | "typeParameter" | "constParameter" | "generic" | "toolModule" => {
            &["type", "type.builtin", "namespace"]
        }
        "function" | "method" => &["function", "function.method", "function.builtin"],
        "macro" | "attributeBracket" | "builtinAttribute" | "decorator" => &[
            "function.macro",
            "function.special",
            "function",
            "attribute",
        ],
        "variable" | "parameter" | "property" | "enumMember" => &[
            "variable",
            "variable.parameter",
            "variable.other.member",
            "variable.builtin",
        ],
        "operator" | "lifetime" => &["operator", "label"],
        _ => &[],
    }
}
