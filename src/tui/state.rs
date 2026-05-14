use crate::loader::LoadedFile;
use camino::Utf8PathBuf;
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;

#[derive(Clone)]
pub struct State {
    pub files: Arc<Vec<LoadedFile>>,
    pub root: Utf8PathBuf,
    pub selected: usize,
    pub open_file: Option<usize>,
    pub preview_scroll: usize,
    pub preview_page_size: usize,
}

impl State {
    pub fn new(files: Arc<Vec<LoadedFile>>, root: Utf8PathBuf) -> Self {
        Self {
            files,
            root,
            selected: 0,
            open_file: None,
            preview_scroll: 0,
            preview_page_size: 0,
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: Arc::new(Vec::new()),
            root: Utf8PathBuf::new(),
            selected: 0,
            open_file: None,
            preview_scroll: 0,
            preview_page_size: 0,
        }
    }
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.files, &other.files)
            && self.selected == other.selected
            && self.open_file == other.open_file
            && self.preview_scroll == other.preview_scroll
    }
}

impl Eq for State {}

impl Debug for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "State {{ files: {}, selected: {}, open_file: {:?} }}",
            self.files.len(),
            self.selected,
            self.open_file
        )
    }
}

impl Display for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "State[files={}, selected={}]",
            self.files.len(),
            self.selected
        )
    }
}

#[derive(Default, Clone, Debug)]
#[non_exhaustive]
pub enum AppSignal {
    SelectNext,
    SelectPrev,
    OpenSelected,
    ScrollPreviewDown(usize),
    ScrollPreviewUp(usize),
    #[default]
    Noop,
}
