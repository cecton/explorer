/// Roots listed on one page of the table of contents.
const TOC_ROOTS_PER_PAGE: usize = 30;
/// Nodes of a straight line listed on one page before it points to the next one.
const TOC_SEGMENT_MAX: usize = 20;

/// Table-of-contents pages for the agent's `conversation` tool. The whole tree is too
/// large to hand over at once, so each page is bounded and names the ids of the pages
/// around it. `current` is the node whose turn is asking.
impl crate::conversation::ConversationTree {
    fn toc_entry(
        &self,
        id: &crate::conversation::NodeId,
        current: &[crate::conversation::NodeId],
        with_size: bool,
    ) -> String {
        use crate::conversation::NodeStatus;
        use crate::conversation::NodeType;
        let Some(node) = self.node(id) else {
            return String::new();
        };
        let kind = node.meta.node_type.map_or("?", NodeType::as_str);
        let mut line = format!("- {} [{kind}", id.as_str());
        match node.meta.status {
            Some(NodeStatus::Done) | None => {}
            Some(status) => line.push_str(&format!(", {}", status.as_str())),
        }
        line.push(']');
        for commit in &node.meta.commits {
            line.push_str(&format!(" {}", commit.chars().take(7).collect::<String>()));
        }
        line.push_str(&format!(" {}", node.display_title()));
        if with_size {
            line.push_str(&format!(" ({} nodes)", self.subtree_len(id)));
        }
        match current.last() {
            Some(asking) if asking == id => line.push_str(" <- this turn"),
            Some(_) if current.contains(id) => line.push_str(" <- your branch"),
            _ => {}
        }
        line.push('\n');
        if let Some(summary) = node.summary.as_deref()
            && summary != node.display_title()
        {
            line.push_str(&format!("  {}\n", crate::util::text::single_line(summary)));
        }
        line
    }

    /// The conversations (root nodes), newest first, `page` starting at 1.
    pub fn toc_roots(&self, page: usize, current: &crate::conversation::NodeId) -> String {
        let lineage = self.lineage(current);
        let pages = self.roots().len().div_ceil(TOC_ROOTS_PER_PAGE).max(1);
        let page = page.clamp(1, pages);
        let mut out = format!(
            "Conversations, newest first (page {page} of {pages}). \
             Use `toc` with a node id to look inside one.\n\n"
        );
        let shown = self
            .roots()
            .iter()
            .rev()
            .skip((page - 1) * TOC_ROOTS_PER_PAGE)
            .take(TOC_ROOTS_PER_PAGE);
        for root in shown {
            out.push_str(&self.toc_entry(root, &lineage, true));
        }
        if page < pages {
            out.push_str(&format!("\nMore conversations: toc page={}\n", page + 1));
        }
        out
    }

    /// One page about `id`: the path down to it, the straight line that follows it, and
    /// the branches where that line forks.
    pub fn toc_node(
        &self,
        id: &crate::conversation::NodeId,
        current: &crate::conversation::NodeId,
    ) -> Option<String> {
        self.node(id)?;
        let lineage = self.lineage(current);
        let path = self.lineage(id);
        let mut out = String::new();
        if let Some((_, ancestors)) = path.split_last()
            && !ancestors.is_empty()
        {
            out.push_str("Path from the root:\n");
            for ancestor in ancestors {
                let title = self.node(ancestor).map_or("", |node| node.display_title());
                out.push_str(&format!("- {} {title}\n", ancestor.as_str()));
            }
            out.push('\n');
        }
        out.push_str("Node:\n");
        out.push_str(&self.toc_entry(id, &lineage, false));

        let mut last = id;
        let mut listed = 0;
        while let Some([only]) = self.node(last).map(|node| node.children.as_slice()) {
            if listed == TOC_SEGMENT_MAX {
                out.push_str(&format!("\nThe line goes on: toc node={}\n", only.as_str()));
                return Some(out);
            }
            if listed == 0 {
                out.push_str("\nContinues with:\n");
            }
            out.push_str(&self.toc_entry(only, &lineage, false));
            listed += 1;
            last = only;
        }
        match self.node(last).map(|node| node.children.as_slice()) {
            Some([]) | None => out.push_str("\nThis branch ends here.\n"),
            Some(children) => {
                out.push_str(&format!("\nBranches after {}:\n", last.as_str()));
                for child in children {
                    out.push_str(&self.toc_entry(child, &lineage, true));
                }
            }
        }
        Some(out)
    }

    /// The node whose turn produced the commit `hash` (full, or a prefix of 7+ characters).
    pub fn node_of_commit(&self, hash: &str) -> Option<&crate::conversation::NodeId> {
        if hash.len() < 7 {
            return None;
        }
        self.nodes()
            .find(|(_, node)| {
                node.meta
                    .commits
                    .iter()
                    .any(|commit| commit.starts_with(hash))
            })
            .map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use crate::conversation::ConversationTree;
    use crate::conversation::NodeId;
    use crate::conversation::test_support::temp_root;
    use crate::conversation::test_support::write_node;

    fn toc_tree() -> (tempfile::TempDir, ConversationTree) {
        let (tmp, root) = temp_root();
        let a = root.join("a1");
        let line = a.join("a2").join("a3");
        write_node(
            &a,
            &[
                ("metadata.md", "type: feedback\nstatus: done\n"),
                ("title.md", "Project goals"),
                ("summary.md", "Goals of the project."),
            ],
        );
        write_node(
            &a.join("a2"),
            &[
                (
                    "metadata.md",
                    "type: commit\ncommits: abcdef0123456789 1234567890abcdef\n",
                ),
                ("title.md", "Write docs"),
            ],
        );
        write_node(&line, &[("title.md", "Fork point")]);
        write_node(
            &line.join("b4"),
            &[("metadata.md", "status: failed\n"), ("title.md", "Left")],
        );
        write_node(&line.join("c4").join("c5"), &[("title.md", "Deep")]);
        write_node(&root.join("z9"), &[("title.md", "Newest root")]);
        (tmp, ConversationTree::load(&root))
    }

    #[test]
    fn toc_roots_lists_conversations_newest_first() {
        let (_tmp, tree) = toc_tree();
        let page = tree.toc_roots(1, &NodeId::new("c5"));
        let newest = page
            .find("- z9 [?] Newest root (1 nodes)")
            .expect("newest root");
        let oldest = page
            .find("- a1 [feedback] Project goals (6 nodes) <- your branch\n  Goals of the project.")
            .expect("oldest root with summary");
        assert!(newest < oldest, "{page}");
        assert!(page.starts_with("Conversations, newest first (page 1 of 1)."));
    }

    #[test]
    fn toc_node_shows_path_line_and_branches() {
        let (_tmp, tree) = toc_tree();
        let page = tree
            .toc_node(&NodeId::new("a2"), &NodeId::new("c5"))
            .unwrap();
        assert_eq!(
            page,
            "Path from the root:\n- a1 Project goals\n\n\
             Node:\n- a2 [commit] abcdef0 1234567 Write docs <- your branch\n\n\
             Continues with:\n- a3 [?] Fork point <- your branch\n\n\
             Branches after a3:\n- b4 [?, failed] Left (1 nodes)\n\
             - c4 [?] (no prompt) (2 nodes) <- your branch\n"
        );
        let leaf = tree
            .toc_node(&NodeId::new("c5"), &NodeId::new("c5"))
            .unwrap();
        assert!(leaf.contains("- c5 [?] Deep <- this turn\n\nThis branch ends here.\n"));
        assert!(
            tree.toc_node(&NodeId::new("nope"), &NodeId::new("c5"))
                .is_none()
        );
    }

    #[test]
    fn toc_node_caps_a_long_line_and_names_the_next_page() {
        let (_tmp, root) = temp_root();
        let mut dir = root.clone();
        for i in 0..25 {
            dir = dir.join(format!("n{i:02}"));
            write_node(&dir, &[]);
        }
        let tree = ConversationTree::load(&root);
        let page = tree
            .toc_node(&NodeId::new("n00"), &NodeId::new("n00"))
            .unwrap();
        assert!(page.contains("- n20 "));
        assert!(!page.contains("- n21 "));
        assert!(page.ends_with("\nThe line goes on: toc node=n21\n"));
    }

    #[test]
    fn node_of_commit_accepts_full_hashes_and_long_prefixes() {
        let (_tmp, tree) = toc_tree();
        let owner = NodeId::new("a2");
        assert_eq!(tree.node_of_commit("1234567890abcdef"), Some(&owner));
        assert_eq!(tree.node_of_commit("abcdef0"), Some(&owner));
        assert_eq!(tree.node_of_commit("abc"), None);
        assert_eq!(tree.node_of_commit("fffffff"), None);
    }
}
