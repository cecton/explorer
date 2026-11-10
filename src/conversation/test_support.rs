//! Builders shared by the conversation modules' test modules.

pub(crate) fn write_node(dir: &camino::Utf8Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
    }
}

pub(crate) fn temp_root() -> (tempfile::TempDir, camino::Utf8PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    (tmp, root)
}

/// A tree with one root node, at `1970-01-02T00-00-01Z`.
pub(crate) fn tree_with_node(
    root: &camino::Utf8Path,
) -> (
    crate::conversation::ConversationTree,
    crate::conversation::NodeId,
) {
    use crate::conversation::metadata::Metadata;
    let mut tree = crate::conversation::ConversationTree::load(root);
    let node = tree
        .create_node(None, 86_401, Metadata::default(), "p", None)
        .unwrap();
    (tree, node)
}
