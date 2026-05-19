use std::path::PathBuf;

pub struct Config {
    pub theme: Option<String>,
}

impl Config {
    pub fn load() -> Result<Option<Self>, String> {
        let path = config_path().ok_or("HOME environment variable not set")?;
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

pub fn save_theme(theme_name: &str) -> Result<(), String> {
    let path = config_path().ok_or("HOME environment variable not set")?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create config directory: {e}"))?;
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(format!(
                "error reading config file '{}': {e}",
                path.display()
            ));
        }
    };

    let mut doc: kdl::KdlDocument = if content.trim().is_empty() {
        kdl::KdlDocument::new()
    } else {
        content.parse().map_err(|e: kdl::KdlError| e.to_string())?
    };

    let mut found = false;
    for node in doc.nodes_mut() {
        if node.name().value() == "theme" {
            node.entries_mut().clear();
            node.push(kdl::KdlEntry::new(theme_name));
            found = true;
            break;
        }
    }

    if !found {
        let mut node = kdl::KdlNode::new("theme");
        node.push(kdl::KdlEntry::new(theme_name));
        doc.nodes_mut().push(node);
    }

    let output = doc.to_string();
    std::fs::write(&path, output)
        .map_err(|e| format!("error writing config file '{}': {e}", path.display()))?;

    Ok(())
}

fn config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        let mut path = PathBuf::from(home);
        path.push(".config/explorer/config.kdl");
        path
    })
}

fn parse(content: &str) -> Result<Config, String> {
    let doc: kdl::KdlDocument = content.parse().map_err(|e: kdl::KdlError| e.to_string())?;

    let mut theme = None;

    for node in doc.nodes() {
        if node.name().value() == "theme" {
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
    }

    Ok(Config { theme })
}
