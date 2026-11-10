//! Test-tree builders shared by the tree's own test modules.

use std::collections::HashSet;

pub(crate) fn tree_with(
    paths: &[&str],
) -> (tempfile::TempDir, crate::conversation::ConversationTree) {
    use crate::conversation::ConversationTree;
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    for path in paths {
        std::fs::create_dir_all(root.join(path)).unwrap();
    }
    let tree = ConversationTree::load(&root);
    (tmp, tree)
}

pub(crate) fn described_tree(
    nodes: &[(&str, &str, &str)],
) -> (tempfile::TempDir, crate::conversation::ConversationTree) {
    use crate::conversation::ConversationTree;
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    for (path, title, summary) in nodes {
        let dir = root.join(path);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("title.md"), title).unwrap();
        if !summary.is_empty() {
            std::fs::write(dir.join("summary.md"), summary).unwrap();
        }
    }
    let tree = ConversationTree::load(&root);
    (tmp, tree)
}

pub(crate) fn ids(names: &[&str]) -> HashSet<crate::conversation::NodeId> {
    use crate::conversation::NodeId;
    names.iter().map(|name| NodeId::new(name)).collect()
}
