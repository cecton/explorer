//! Builders shared by the test modules of every layer.

/// A fresh temporary directory and its canonical UTF-8 path.
pub(crate) fn temp_root() -> (tempfile::TempDir, camino::Utf8PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().canonicalize().unwrap()).unwrap();
    (tmp, root)
}
