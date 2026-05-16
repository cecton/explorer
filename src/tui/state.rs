use crate::loader::LoadedFile;
use crate::supervisor::TaskStatus;
use crate::watcher::BatchedWatchEvent;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use r3bl_tui::{EditorBuffer, FlexBoxId, HasEditorBuffers};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static FILES_VERSION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct State {
    pub files: Arc<ArcSwap<Vec<LoadedFile>>>,
    /// Incremented whenever file contents change so PartialEq detects mutations.
    pub files_version: u64,
    pub root: Utf8PathBuf,
    pub open_file: Option<usize>,
    pub preview_scroll: usize,
    pub preview_page_size: usize,
    pub file_name_picker_open: bool,
    /// Each entry: (index into files snapshot, sorted+deduped matched char positions from nucleo).
    pub file_name_picker_results: Vec<(usize, Vec<u32>)>,
    pub file_name_picker_selected: usize,
    /// Last known status for each supervised task, keyed by task name.
    pub task_statuses: Vec<(&'static str, TaskStatus)>,
    pub editor_buffers: HashMap<FlexBoxId, EditorBuffer>,
}

impl HasEditorBuffers for State {
    fn get_mut_editor_buffer(&mut self, id: FlexBoxId) -> Option<&mut EditorBuffer> {
        self.editor_buffers.get_mut(&id)
    }

    fn insert_editor_buffer(&mut self, id: FlexBoxId, buffer: EditorBuffer) {
        self.editor_buffers.insert(id, buffer);
    }

    fn contains_editor_buffer(&self, id: FlexBoxId) -> bool {
        self.editor_buffers.contains_key(&id)
    }
}

impl State {
    pub fn bump_files_version(&mut self) {
        self.files_version = FILES_VERSION.fetch_add(1, Ordering::Relaxed) + 1;
    }

    pub fn set_task_status(&mut self, name: &'static str, status: TaskStatus) {
        if let Some(entry) = self.task_statuses.iter_mut().find(|(n, _)| *n == name) {
            entry.1 = status;
        } else {
            self.task_statuses.push((name, status));
        }
    }

    /// Returns a short status string for tasks that are not Running, or empty if all healthy.
    pub fn task_status_line(&self) -> String {
        let parts: Vec<String> = self
            .task_statuses
            .iter()
            .filter_map(|(name, status)| match status {
                TaskStatus::Restarting => Some(format!("{name}: restarting")),
                TaskStatus::Running => None,
            })
            .collect();
        parts.join(", ")
    }
}

impl State {
    pub fn new(files: Arc<ArcSwap<Vec<LoadedFile>>>, root: Utf8PathBuf) -> Self {
        let snapshot = files.load();
        let file_name_picker_results = (0..snapshot.len()).map(|i| (i, vec![])).collect();
        Self {
            files,
            files_version: 0,
            root,
            open_file: None,
            preview_scroll: 0,
            preview_page_size: 0,
            file_name_picker_open: true,
            file_name_picker_results,
            file_name_picker_selected: 0,
            task_statuses: Vec::new(),
            editor_buffers: HashMap::new(),
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: Arc::new(ArcSwap::from_pointee(Vec::new())),
            files_version: 0,
            root: Utf8PathBuf::new(),
            open_file: None,
            preview_scroll: 0,
            preview_page_size: 0,
            file_name_picker_open: false,
            file_name_picker_results: Vec::new(),
            file_name_picker_selected: 0,
            task_statuses: Vec::new(),
            editor_buffers: HashMap::new(),
        }
    }
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.files, &other.files)
            && self.files_version == other.files_version
            && self.open_file == other.open_file
            && self.preview_scroll == other.preview_scroll
            && self.file_name_picker_open == other.file_name_picker_open
            && self.file_name_picker_selected == other.file_name_picker_selected
    }
}

impl Eq for State {}

impl Debug for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let count = self.files.load().len();
        write!(
            f,
            "State {{ files: {}, open_file: {:?}, picker_open: {} }}",
            count, self.open_file, self.file_name_picker_open
        )
    }
}

impl Display for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "State[files={}]", self.files.load().len())
    }
}

#[derive(Default, Clone, Debug)]
#[non_exhaustive]
pub enum AppSignal {
    OpenFileNamePicker,
    CloseFileNamePicker,
    FileNamePickerQueryChanged,
    FileNamePickerSelectNext,
    FileNamePickerSelectPrev,
    FileNamePickerConfirm,
    ScrollPreviewDown(usize),
    ScrollPreviewUp(usize),
    FilesChanged(Arc<BatchedWatchEvent>),
    TaskRestarting(&'static str),
    TaskRunning(&'static str),
    #[default]
    Noop,
}
