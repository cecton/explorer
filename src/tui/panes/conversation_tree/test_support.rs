//! Test-tree builders shared by the tree's own test modules.

use std::collections::HashSet;

pub(crate) fn tree_with(
    paths: &[&str],
) -> (tempfile::TempDir, crate::conversation::ConversationTree) {
    use crate::conversation::ConversationTree;
    use crate::util::test_support::temp_root;
    let (tmp, root) = temp_root();
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
    use crate::conversation::test_support::write_node;
    use crate::util::test_support::temp_root;
    let (tmp, root) = temp_root();
    for (path, title, summary) in nodes {
        let mut files = vec![(crate::conversation::files::TITLE, *title)];
        if !summary.is_empty() {
            files.push((crate::conversation::files::SUMMARY, *summary));
        }
        write_node(&root.join(path), &files);
    }
    let tree = ConversationTree::load(&root);
    (tmp, tree)
}

pub(crate) fn ids(names: &[&str]) -> HashSet<crate::conversation::NodeId> {
    use crate::conversation::NodeId;
    names.iter().map(|name| NodeId::new(name)).collect()
}
