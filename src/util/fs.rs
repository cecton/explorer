pub fn write_atomic(path: &camino::Utf8Path, content: &str) -> std::io::Result<()> {
    let tmp: camino::Utf8PathBuf = format!("{path}.tmp").into();
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

pub(crate) fn data_dir() -> Option<camino::Utf8PathBuf> {
    directories::ProjectDirs::from("", "", "explorer")
        .and_then(|dirs| camino::Utf8PathBuf::from_path_buf(dirs.data_dir().to_path_buf()).ok())
}

pub(crate) fn config_dir() -> Option<camino::Utf8PathBuf> {
    directories::ProjectDirs::from("", "", "explorer")
        .and_then(|dirs| camino::Utf8PathBuf::from_path_buf(dirs.config_dir().to_path_buf()).ok())
}

/// Stable 16-hex identifier of a repository, derived from its canonical root path.
pub fn repo_hash(repo_root: &camino::Utf8Path) -> Option<String> {
    use sha2::Digest;
    let canonical = std::fs::canonicalize(repo_root).ok()?;
    let canonical = camino::Utf8PathBuf::from_path_buf(canonical).ok()?;
    let hash = sha2::Sha256::digest(canonical.as_str().as_bytes());
    Some(hex::encode(&hash[..8]))
}

/// Resolves `path` against the repository root and refuses anything outside it.
pub fn resolve_in(root: &camino::Utf8Path, path: &str) -> Result<camino::Utf8PathBuf, String> {
    let joined = if camino::Utf8Path::new(path).is_absolute() {
        camino::Utf8PathBuf::from(path)
    } else {
        root.join(path)
    };
    let mut resolved = camino::Utf8PathBuf::new();
    for component in joined.components() {
        match component {
            camino::Utf8Component::CurDir => {}
            camino::Utf8Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_str()),
        }
    }
    if resolved.starts_with(root) {
        Ok(resolved)
    } else {
        Err(format!("{path} is outside the repository"))
    }
}

/// The items `parse` makes from the `.md` files in `dir`, sorted by `name_of`.
pub(crate) fn read_markdown<T>(
    dir: &std::path::Path,
    kind: &str,
    parse: impl Fn(&str, &str) -> Result<T, String>,
    name_of: impl Fn(&T) -> &str,
) -> Vec<T> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<T> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "md" {
                return None;
            }
            let name = path.file_stem()?.to_str()?;
            if !is_markdown_name(name) {
                tracing::warn!(
                    "ignoring {}: {kind} name is made of lowercase letters, digits and dashes",
                    path.display()
                );
                return None;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| tracing::warn!("cannot read {}: {e}", path.display()))
                .ok()?;
            parse(name, &text)
                .map_err(|reason| tracing::warn!("ignoring {}: {reason}", path.display()))
                .ok()
        })
        .collect();
    found.sort_by(|a, b| name_of(a).cmp(name_of(b)));
    found
}

/// A name made of lowercase letters, digits and dashes: the file stems of every
/// directory Explorer reads for markdown.
pub(crate) fn is_markdown_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_refuses_paths_outside_the_root() {
        let root = camino::Utf8Path::new("/repo");
        assert_eq!(resolve_in(root, "src/../a.rs").unwrap(), "/repo/a.rs");
        assert_eq!(resolve_in(root, "/repo/b.rs").unwrap(), "/repo/b.rs");
        assert!(resolve_in(root, "../etc/passwd").is_err());
        assert!(resolve_in(root, "/etc/passwd").is_err());
        assert!(resolve_in(root, "src/../../x").is_err());
    }

    #[test]
    fn markdown_is_read_sorted_and_named_by_the_reader() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("20-b.md"), "second").unwrap();
        std::fs::write(dir.join("10-a.md"), "first").unwrap();
        std::fs::write(dir.join("Bad Name.md"), "x").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();

        let found = read_markdown(
            dir,
            "a thing",
            |name, text| Ok((name.to_string(), text.to_string())),
            |(name, _)| name,
        );
        assert_eq!(
            found,
            [
                ("10-a".to_string(), "first".to_string()),
                ("20-b".to_string(), "second".to_string())
            ]
        );
        let missing = dir.join("missing");
        assert!(read_markdown(missing.as_path(), "a thing", |_, _| Ok(()), |_| "").is_empty());
    }

    #[test]
    fn a_thing_that_will_not_parse_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("bad.md"), "x").unwrap();
        let found = read_markdown(
            tmp.path(),
            "a thing",
            |_, _| Err("it is bad".to_string()),
            |name: &String| name,
        );
        assert!(found.is_empty());
    }
}
