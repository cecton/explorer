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
}
