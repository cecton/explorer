use std::path::PathBuf;

pub struct Config {
    /// Fallback theme used only when a project has no session (or no session-saved
    /// theme) yet. Theme selection normally lives in the session; this can only be
    /// changed by hand-editing the config file, not from the UI.
    pub theme: Option<String>,
    /// `(action-slug, key-spec)` pairs from the `keybindings` block, applied on top of the
    /// default keymap by [`crate::keymap::Keymap::apply_overrides`].
    pub keybindings: Vec<(String, String)>,
    /// Settings for the status-bar time counters, from the `counters { … }` block.
    pub counters: CountersConfig,
}

/// Settings for the status-bar session/project time counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountersConfig {
    /// Whether to display the counters on the right of the status bar (`show`).
    pub show: bool,
    /// Whether terminal output ("stuff moving") counts as active time even without
    /// keyboard input (`render-counts-as-active`).
    pub render_counts_as_active: bool,
}

impl Default for CountersConfig {
    fn default() -> Self {
        Self {
            show: true,
            render_counts_as_active: true,
        }
    }
}

impl Config {
    pub fn load() -> Result<Option<Self>, String> {
        let path = config_path().ok_or("could not determine config directory")?;
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "error reading config file '{}': {e}",
                    path.display()
                ));
            }
        };

        parse(&content).map(Some)
    }
}

fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "explorer").map(|dirs| {
        let mut path = dirs.config_dir().to_path_buf();
        path.push("config.kdl");
        path
    })
}

fn parse(content: &str) -> Result<Config, String> {
    let doc: kdl::KdlDocument = content.parse().map_err(|e: kdl::KdlError| e.to_string())?;

    let mut theme = None;
    let mut keybindings = Vec::new();
    let mut counters = CountersConfig::default();

    for node in doc.nodes() {
        match node.name().value() {
            "counters" => {
                let Some(children) = node.children() else {
                    continue;
                };
                for child in children.nodes() {
                    let name = child.name().value();
                    let val = child
                        .entries()
                        .iter()
                        .find(|e| e.name().is_none())
                        .and_then(|e| e.value().as_bool())
                        .ok_or_else(|| format!("counters.{name} must have a boolean argument"))?;
                    match name {
                        "show" => counters.show = val,
                        "render-counts-as-active" => counters.render_counts_as_active = val,
                        _ => {}
                    }
                }
            }
            "theme" => {
                if let Some(arg) = node.entries().iter().find(|e| e.name().is_none()) {
                    if let Some(val) = arg.value().as_string() {
                        theme = Some(val.to_string());
                    } else {
                        return Err("theme node argument must be a string".to_string());
                    }
                } else {
                    return Err("theme node must have a string argument".to_string());
                }
            }
            "keybindings" => {
                let Some(children) = node.children() else {
                    continue;
                };
                for bind in children.nodes() {
                    let slug = bind.name().value().to_string();
                    let arg = bind
                        .entries()
                        .iter()
                        .find(|e| e.name().is_none())
                        .and_then(|e| e.value().as_string())
                        .ok_or_else(|| {
                            format!("keybinding '{slug}' must have a string key argument")
                        })?;
                    keybindings.push((slug, arg.to_string()));
                }
            }
            _ => {}
        }
    }

    Ok(Config {
        theme,
        keybindings,
        counters,
    })
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn config_path_uses_project_dirs() {
        let path = config_path().expect("config_path should return Some");
        let file_name = path.file_name().expect("path has file name");
        assert_eq!(file_name, "config.kdl");
        assert!(path.to_string_lossy().contains("explorer"));
    }

    #[test]
    fn parses_theme_and_keybindings_block() {
        let content = r#"
            theme "night_owl"
            keybindings {
                quit "Q"
                open-terminal "t"
                resize-grow "ctrl+down"
                leader "alt+`"
            }
        "#;
        let config = parse(content).expect("valid config");
        assert_eq!(config.theme.as_deref(), Some("night_owl"));
        assert_eq!(
            config.keybindings,
            vec![
                ("quit".to_string(), "Q".to_string()),
                ("open-terminal".to_string(), "t".to_string()),
                ("resize-grow".to_string(), "ctrl+down".to_string()),
                ("leader".to_string(), "alt+`".to_string()),
            ]
        );
    }

    #[test]
    fn counters_default_when_block_absent() {
        let config = parse("theme \"night_owl\"").expect("valid config");
        assert_eq!(config.counters, CountersConfig::default());
        assert!(config.counters.show);
        assert!(config.counters.render_counts_as_active);
    }

    #[test]
    fn counters_block_toggles_both_options() {
        let config = parse("counters {\n    show #false\n    render-counts-as-active #false\n}")
            .expect("valid config");
        assert!(!config.counters.show);
        assert!(!config.counters.render_counts_as_active);
    }

    #[test]
    fn counters_block_partial_keeps_other_default() {
        // Only `show` set -> render-counts-as-active stays at its default (true).
        let config = parse("counters {\n    show #false\n}").expect("valid config");
        assert!(!config.counters.show);
        assert!(config.counters.render_counts_as_active);
    }

    #[test]
    fn counters_option_requires_a_boolean_argument() {
        assert!(parse("counters {\n    show \"yes\"\n}").is_err());
        assert!(parse("counters {\n    show\n}").is_err());
    }

    #[test]
    fn counters_ignores_unknown_child_nodes() {
        let config = parse("counters {\n    show #false\n    wat #true\n}").expect("valid config");
        assert!(!config.counters.show);
    }

    #[test]
    fn keybindings_are_empty_when_absent() {
        let config = parse("theme \"night_owl\"").expect("valid config");
        assert!(config.keybindings.is_empty());
    }

    #[test]
    fn keybinding_without_key_argument_is_an_error() {
        assert!(parse("keybindings {\n quit\n }").is_err());
    }
}
