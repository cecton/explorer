//! Builders shared by the conversation modules' test modules.

pub(crate) fn write_node(dir: &camino::Utf8Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
    }
}
