/// The color class of a semantic token: one variant per distinct set of theme scopes a
/// token type maps to, so a token type the server invents costs nothing but a fallback.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TokenKind {
    Keyword,
    Modifier,
    SelfKeyword,
    Comment,
    String,
    Number,
    Constant,
    Namespace,
    Type,
    Function,
    Macro,
    Variable,
    Parameter,
    Property,
    Operator,
    Lifetime,
    #[default]
    Other,
}

impl TokenKind {
    pub const COUNT: usize = 17;

    pub const ALL: [Self; Self::COUNT] = [
        Self::Keyword,
        Self::Modifier,
        Self::SelfKeyword,
        Self::Comment,
        Self::String,
        Self::Number,
        Self::Constant,
        Self::Namespace,
        Self::Type,
        Self::Function,
        Self::Macro,
        Self::Variable,
        Self::Parameter,
        Self::Property,
        Self::Operator,
        Self::Lifetime,
        Self::Other,
    ];

    /// The theme scopes of a token kind, most specific first: the first one the theme
    /// styles gives the token its color. Later entries are fallbacks for the bundled
    /// themes that style only one member of a family (`type.builtin` without `type`,
    /// `constant.numeric` without `constant`, …).
    pub fn scopes(self) -> &'static [&'static str] {
        match self {
            Self::Keyword => &["keyword", "keyword.control"],
            Self::Modifier => &["keyword.storage.modifier", "keyword"],
            Self::SelfKeyword => &["keyword", "variable.builtin"],
            Self::Comment => &["comment"],
            Self::String => &["string", "comment"],
            Self::Number => &["constant.numeric", "constant"],
            Self::Constant => &["constant", "constant.builtin", "constant.numeric"],
            Self::Namespace => &["namespace", "type", "type.builtin"],
            Self::Type => &["type", "type.builtin"],
            Self::Function => &["function", "function.method", "function.builtin"],
            Self::Macro => &[
                "function.macro",
                "function.special",
                "attribute",
                "function",
            ],
            Self::Variable => &["variable"],
            Self::Parameter => &[
                "variable.parameter",
                "variable",
                "variable.other.member",
                "variable.builtin",
            ],
            Self::Property => &["variable.other.member", "variable"],
            Self::Operator => &["operator"],
            Self::Lifetime => &["label", "operator"],
            Self::Other => &[],
        }
    }

    /// The kind of a legend entry, matched as the server writes it: a token type Explorer
    /// has no class for is `Other`.
    pub fn from_name(name: &str) -> Self {
        match name {
            "keyword" => Self::Keyword,
            "modifier" => Self::Modifier,
            "selfKeyword" | "boolean" => Self::SelfKeyword,
            "comment" => Self::Comment,
            "string" | "character" | "escapeSequence" => Self::String,
            "number" => Self::Number,
            "const" | "static" => Self::Constant,
            "namespace" => Self::Namespace,
            "type" | "class" | "struct" | "enum" | "interface" | "builtinType" | "typeAlias"
            | "typeParameter" | "constParameter" | "generic" | "toolModule" => Self::Type,
            "function" | "method" => Self::Function,
            "macro" | "attributeBracket" | "builtinAttribute" | "decorator" => Self::Macro,
            "variable" => Self::Variable,
            "parameter" => Self::Parameter,
            "property" | "enumMember" => Self::Property,
            "operator" => Self::Operator,
            "lifetime" => Self::Lifetime,
            _ => Self::Other,
        }
    }
}

/// One run of a line that shares a semantic token kind, as byte offsets into the line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ColoredSpan {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

pub type ColoredLine = Vec<ColoredSpan>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_variant_in_declaration_order() {
        assert_eq!(TokenKind::ALL.len(), TokenKind::COUNT);
        let kinds: Vec<TokenKind> = TokenKind::ALL.to_vec();
        let mut sorted = kinds.clone();
        sorted.sort_by_key(|kind| *kind as usize);
        assert_eq!(kinds, sorted);
        // The discriminants must be `0..COUNT`: `token_color` indexes its table by them.
        let discriminants: Vec<usize> = kinds.iter().map(|kind| *kind as usize).collect();
        assert_eq!(discriminants, (0..TokenKind::COUNT).collect::<Vec<_>>());
    }

    /// Every legend name the LSP layer can meet keeps the exact scope list it had when
    /// the mapping lived in the theme, so no token type silently loses its fallbacks.
    #[test]
    fn every_legend_name_keeps_its_scopes() {
        let table: [(&str, &[&str]); 35] = [
            ("keyword", &["keyword", "keyword.control"]),
            ("modifier", &["keyword.storage.modifier", "keyword"]),
            ("selfKeyword", &["keyword", "variable.builtin"]),
            ("boolean", &["keyword", "variable.builtin"]),
            ("comment", &["comment"]),
            ("string", &["string", "comment"]),
            ("character", &["string", "comment"]),
            ("escapeSequence", &["string", "comment"]),
            ("number", &["constant.numeric", "constant"]),
            (
                "const",
                &["constant", "constant.builtin", "constant.numeric"],
            ),
            (
                "static",
                &["constant", "constant.builtin", "constant.numeric"],
            ),
            ("namespace", &["namespace", "type", "type.builtin"]),
            ("type", &["type", "type.builtin"]),
            ("class", &["type", "type.builtin"]),
            ("struct", &["type", "type.builtin"]),
            ("enum", &["type", "type.builtin"]),
            ("interface", &["type", "type.builtin"]),
            ("builtinType", &["type", "type.builtin"]),
            ("typeAlias", &["type", "type.builtin"]),
            ("typeParameter", &["type", "type.builtin"]),
            ("constParameter", &["type", "type.builtin"]),
            ("generic", &["type", "type.builtin"]),
            ("toolModule", &["type", "type.builtin"]),
            (
                "function",
                &["function", "function.method", "function.builtin"],
            ),
            (
                "method",
                &["function", "function.method", "function.builtin"],
            ),
            (
                "macro",
                &[
                    "function.macro",
                    "function.special",
                    "attribute",
                    "function",
                ],
            ),
            (
                "attributeBracket",
                &[
                    "function.macro",
                    "function.special",
                    "attribute",
                    "function",
                ],
            ),
            (
                "builtinAttribute",
                &[
                    "function.macro",
                    "function.special",
                    "attribute",
                    "function",
                ],
            ),
            (
                "decorator",
                &[
                    "function.macro",
                    "function.special",
                    "attribute",
                    "function",
                ],
            ),
            ("variable", &["variable"]),
            (
                "parameter",
                &[
                    "variable.parameter",
                    "variable",
                    "variable.other.member",
                    "variable.builtin",
                ],
            ),
            ("property", &["variable.other.member", "variable"]),
            ("enumMember", &["variable.other.member", "variable"]),
            ("operator", &["operator"]),
            ("lifetime", &["label", "operator"]),
        ];
        for (name, scopes) in table {
            assert_eq!(TokenKind::from_name(name).scopes(), scopes, "{name}");
        }
    }

    #[test]
    fn an_unknown_name_is_other() {
        assert_eq!(TokenKind::from_name("nonsense"), TokenKind::Other);
        assert_eq!(TokenKind::from_name(""), TokenKind::Other);
    }

    #[test]
    fn other_styles_nothing() {
        assert!(TokenKind::Other.scopes().is_empty());
        assert_eq!(TokenKind::default(), TokenKind::Other);
    }
}
