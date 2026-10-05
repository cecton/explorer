use crate::git::GitRef;
use crate::git::LogEntry;
use crate::git::RefKind;
use crate::git::commit_type_prefix;
use crate::git::format_log_time;
use crate::tui::*;
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Utf32Str};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct GitLogPaneComponent {
    id: FlexBoxId,
    window_id: usize,
    content_origin_row: usize,
    content_row_count: usize,
    search_active: bool,
    input_line: InputLine,
    branch_input_line: InputLine,
    branch_fuzzy: FuzzyPicker,
}

impl GitLogPaneComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            window_id: 0,
            content_origin_row: 0,
            content_row_count: 0,
            search_active: false,
            input_line: InputLine::new(),
            branch_input_line: InputLine::new(),
            branch_fuzzy: FuzzyPicker::new(),
        }
    }

    pub fn set_window_id(&mut self, window_id: usize) {
        self.window_id = window_id;
    }

    pub(crate) fn open_commit_diff(
        state: &mut AppState,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
        id: [u8; 20],
    ) {
        if let std::collections::hash_map::Entry::Vacant(slot) = state.commit_diffs.entry(id) {
            slot.insert(CommitDiffState::default());
            match state.repo.clone() {
                Some(repo) => {
                    tokio::task::spawn_blocking(move || {
                        let result = crate::git::load_commit_diff(&repo, id)
                            .map(|text| Arc::new(crate::git::classify_diff(&text)));
                        send_signal!(
                            main_tx,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::CommitDiffLoaded { id, result }
                            )
                        );
                    });
                }
                None => {
                    send_signal!(
                        main_tx,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::CommitDiffLoaded {
                                id,
                                result: Err("open repository: not a git repository".to_string()),
                            }
                        )
                    );
                }
            }
        }
        let old = state.pane_manager.focused_window;
        let window = Window::CommitDiff(id);
        state.pane_manager.push_window(window);
        state.pane_manager.focused_window = Some(window);
        crate::tui::app::notify_terminal_focus_change(state, old, Some(window));
        state.mark_session_dirty();
    }

    pub(crate) fn reload_log(
        repo: Option<gix::ThreadSafeRepository>,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
        id: usize,
        branch: Option<String>,
    ) {
        let Some(repo) = repo else {
            send_signal!(
                main_tx,
                TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::GitLogLoaded {
                    id,
                    result: Err("open repository: not a git repository".to_string()),
                })
            );
            return;
        };
        tokio::task::spawn_blocking(move || {
            let result = crate::git::load_log(&repo, branch.as_deref()).map(Arc::new);
            send_signal!(
                main_tx,
                TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::GitLogLoaded {
                    id,
                    result
                })
            );
        });
    }

    fn refresh_branch_results(git_refs: &Option<Arc<Vec<GitRef>>>, win: &mut GitLogWindowState) {
        let Some(refs) = git_refs.as_ref() else {
            return;
        };
        win.branch_picker.results = run_ref_match(&win.branch_picker.query, refs);
        win.branch_picker.selected = win.branch_picker.results.first().map(|(k, _)| *k);
    }

    pub(crate) fn refresh_branch_results_for_window(state: &mut AppState, id: usize) {
        if !state.git_logs.get(&id).is_some_and(|w| w.branch_picking) {
            return;
        }
        let AppState {
            git_logs, git_refs, ..
        } = state;
        let Some(win) = git_logs.get_mut(&id) else {
            return;
        };
        Self::refresh_branch_results(git_refs, win);
    }

    fn move_selection(win: &mut GitLogWindowState, target: usize, count: usize) -> bool {
        if count == 0 {
            return false;
        }
        let clamped = target.min(count - 1);
        if clamped != win.selected {
            win.selected = clamped;
            return true;
        }
        false
    }
}

impl GitLogPaneComponent {
    pub fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
        window_id: usize,
    ) -> usize {
        let Some(win) = state.git_logs.get(&window_id) else {
            return 0;
        };
        let input_mode = if win.branch_picking {
            Some((
                &self.branch_input_line,
                &win.branch_picker.query,
                "Show branch:",
            ))
        } else if self.search_active {
            Some((&self.input_line, &win.query, "/"))
        } else {
            None
        };
        if let Some((input_line, query, prompt)) = input_mode {
            let origin = pane_box.style_adjusted_origin_pos;
            let width = pane_box.style_adjusted_bounds_size.col_width.as_u16();
            let (bg_rgb, fg_rgb) = title_bar_colors(focused, theme);
            let color_bg = TuiColor::from(bg_rgb);
            let color_fg = TuiColor::from(fg_rgb);
            let bg_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});
            *ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
            *ops += RenderOpCommon::SetBgColor(color_bg);
            *ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(width as usize).as_str().into(),
                Some(bg_style),
            );
            input_line.render(
                ops,
                query,
                prompt,
                origin,
                width,
                focused,
                (color_bg, color_fg),
            );
            1
        } else {
            let mut title = match win.branch.as_deref() {
                Some(branch) => format!("Logs ({branch})"),
                None => "Logs".to_string(),
            };
            if win
                .entries
                .as_ref()
                .is_some_and(|history| history.truncated)
            {
                title.push_str(&format!(" ({})", crate::git::MAX_LOG_ENTRIES));
                title.push('+');
            }
            if !win.query.is_empty() {
                title.push_str(&format!(" — {}", win.query));
            }
            render_pane_title(ops, pane_box, &title, false, theme, focused);
            1
        }
    }
}

impl Component<AppState, AppSignal> for GitLogPaneComponent {
    fn reset(&mut self) {
        self.search_active = false;
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        let window_id = self.window_id;
        let main_tx = global_data.main_thread_channel_sender.clone();
        let state = &mut global_data.state;
        if !state.git_logs.contains_key(&window_id) {
            return Ok(EventPropagation::Propagate);
        }
        let AppState {
            git_logs,
            git_refs,
            pane_manager,
            repo,
            ..
        } = state;
        let win = git_logs.get_mut(&window_id).expect("checked above");

        if let InputEvent::Keyboard(KeyPress::Plain {
            key: Key::Character('/'),
        }) = input_event
            && !self.search_active
            && !win.branch_picking
        {
            self.search_active = true;
            self.input_line.set_cursor_end(&win.query);
            if win.query.is_empty() {
                win.filtered = None;
            } else {
                apply_filter(win);
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        if let InputEvent::Keyboard(KeyPress::Plain {
            key: Key::Character('b'),
        }) = input_event
            && !self.search_active
            && !win.branch_picking
        {
            win.branch_picking = true;
            win.branch_picker.query.clear();
            win.branch_picker.results.clear();
            win.branch_picker.selected = None;
            self.branch_input_line.set_cursor_end("");
            if git_refs.is_some() {
                Self::refresh_branch_results(git_refs, win);
            } else if let Some(repo) = repo.clone() {
                let picker_tx = main_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = crate::git::load_refs(&repo).map(Arc::new);
                    send_signal!(
                        picker_tx,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::GitRefsLoaded(
                            result
                        ))
                    );
                });
            } else {
                send_signal!(
                    main_tx,
                    TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::GitRefsLoaded(Err(
                        "open repository: not a git repository".to_string()
                    )))
                );
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        if win.branch_picking {
            match &input_event {
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Esc),
                }) => {
                    win.branch_picking = false;
                    win.branch_picker = FuzzyPickerState::default();
                    win.branch = None;
                    Self::reload_log(repo.clone(), main_tx, window_id, None);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Enter),
                }) => {
                    let picked = match win.branch_picker.selected {
                        Some(idx) => win
                            .branch_picker
                            .results
                            .iter()
                            .find(|(k, _)| *k == idx)
                            .map(|(k, _)| *k),
                        None => win.branch_picker.results.first().map(|(k, _)| *k),
                    }
                    .and_then(|k| git_refs.as_ref().and_then(|refs| refs.get(k)))
                    .map(|git_ref| git_ref.name.clone());
                    if let Some(name) = picked {
                        win.branch_picking = false;
                        win.branch_picker = FuzzyPickerState::default();
                        win.branch = Some(name.clone());
                        win.query.clear();
                        win.filtered = None;
                        win.selected = 0;
                        Self::reload_log(repo.clone(), main_tx, window_id, Some(name));
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key:
                        Key::SpecialKey(SpecialKey::Up)
                        | Key::SpecialKey(SpecialKey::Down)
                        | Key::SpecialKey(SpecialKey::PageUp)
                        | Key::SpecialKey(SpecialKey::PageDown),
                })
                | InputEvent::Mouse(MouseInput {
                    kind: MouseInputKind::ScrollUp | MouseInputKind::ScrollDown,
                    ..
                }) => {
                    self.branch_fuzzy.handle_navigation(
                        &input_event,
                        self.content_row_count.max(1),
                        &mut win.branch_picker,
                    );
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Mouse(MouseInput {
                    kind: MouseInputKind::MouseDown(Button::Left),
                    pos,
                    ..
                }) => {
                    let rel = pos
                        .row_index
                        .as_usize()
                        .saturating_sub(self.content_origin_row);
                    let idx = self.branch_fuzzy.scroll_offset + rel;
                    if let Some((k, _)) = win.branch_picker.results.get(idx) {
                        win.branch_picker.selected = Some(*k);
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
            if self
                .branch_input_line
                .handle_key(&input_event, &mut win.branch_picker.query)
            {
                Self::refresh_branch_results(git_refs, win);
                return Ok(EventPropagation::ConsumedRender);
            }
            return Ok(EventPropagation::Propagate);
        }

        let mut open_commit = None;
        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) if self.search_active => {
                win.query.clear();
                apply_filter(win);
                self.search_active = false;
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Backspace),
            }) if !self.search_active => {
                pane_manager.send_to_back(&Window::GitLog(window_id));
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                if self.search_active {
                    self.search_active = false;
                    return Ok(EventPropagation::ConsumedRender);
                }
                open_commit = visible_entry_id(win);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Up),
            }) if Self::move_selection(win, win.selected.saturating_sub(1), visible_len(win)) => {
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Down),
            }) if Self::move_selection(win, win.selected + 1, visible_len(win)) => {
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageUp),
            }) => {
                let page = pane_manager
                    .window_page_size(&Window::GitLog(window_id))
                    .saturating_sub(1)
                    .max(1);
                if Self::move_selection(win, win.selected.saturating_sub(page), visible_len(win)) {
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageDown),
            }) => {
                let page = pane_manager
                    .window_page_size(&Window::GitLog(window_id))
                    .saturating_sub(1)
                    .max(1);
                if Self::move_selection(win, win.selected + page, visible_len(win)) {
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Home),
            }) if Self::move_selection(win, 0, visible_len(win)) => {
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::End),
            }) if Self::move_selection(
                win,
                visible_len(win).saturating_sub(1),
                visible_len(win),
            ) =>
            {
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::MouseDown(Button::Left),
                pos,
                ..
            }) => {
                let row = pos.row_index.as_usize();
                if row >= self.content_origin_row
                    && row < self.content_origin_row + self.content_row_count
                {
                    let rel = row - self.content_origin_row;
                    let scroll = pane_manager.window_scroll(&Window::GitLog(window_id));
                    if Self::move_selection(win, scroll + rel, visible_len(win)) {
                        return Ok(EventPropagation::ConsumedRender);
                    }
                }
            }
            _ => {}
        }

        if self.search_active && self.input_line.handle_key(&input_event, &mut win.query) {
            apply_filter(win);
            return Ok(EventPropagation::ConsumedRender);
        }

        if let Some(commit_id) = open_commit {
            Self::open_commit_diff(state, main_tx, commit_id);
            return Ok(EventPropagation::ConsumedRender);
        }

        Ok(EventPropagation::Propagate)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        throws!({
            let window_id = self.window_id;
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();
            self.content_origin_row = origin.row_index.as_usize();
            self.content_row_count = total_rows;

            let window = Window::GitLog(window_id);
            let state = &mut global_data.state;
            state.pane_manager.set_window_page_size(&window, total_rows);

            let (entries, selected, error, filtering, branch_picking) =
                match state.git_logs.get(&window_id) {
                    Some(win) => (
                        win.entries.clone(),
                        win.selected,
                        win.error.clone(),
                        win.filtered.is_some(),
                        win.branch_picking,
                    ),
                    None => (None, 0, None, false, false),
                };
            let visible = state.git_logs.get(&window_id).map(visible_len).unwrap_or(0);
            state.pane_manager.set_window_scroll_max(&window, visible);
            state.pane_manager.clamp_scroll(&window);

            let mut scroll = state.pane_manager.window_scroll(&window);
            if visible > 0 && total_rows > 0 {
                if selected < scroll {
                    scroll = selected;
                } else if selected >= scroll + total_rows {
                    scroll = selected + 1 - total_rows;
                }
                state.pane_manager.set_window_scroll(&window, scroll);
            }

            let pane_bg = state.theme.ui_bg("ui.background").unwrap_or(DEFAULT_BG);
            let bg = TuiColor::from(pane_bg);
            let bg_style = new_style!(color_bg: {bg});
            let selected_bg = TuiColor::from(
                state
                    .theme
                    .ui_bg("ui.selection")
                    .unwrap_or(DEFAULT_SELECTION_BG),
            );
            let normal_fg = TuiColor::from(
                state
                    .theme
                    .ui_fg("ui.text")
                    .unwrap_or(RgbValue::from_u8(170, 170, 200)),
            );
            let error_fg = TuiColor::from(
                state
                    .theme
                    .ui_fg("error")
                    .unwrap_or(RgbValue::from_u8(220, 80, 80)),
            );
            let selected_text_style = new_style!(color_fg: {normal_fg} color_bg: {selected_bg});

            let mut render_ops = RenderOpIRVec::new();

            if branch_picking {
                let branch_rgb = state
                    .theme
                    .color_for_scope("namespace")
                    .unwrap_or(RgbValue::from_u8(100, 160, 255));
                let tag_rgb = match state.theme.color_for_scope("special") {
                    Some(rgb) if rgb != branch_rgb => rgb,
                    _ => RgbValue::from_u8(255, 170, 80),
                };
                if let Some(win) = state.git_logs.get(&window_id) {
                    let results_origin =
                        col(origin.col_index.as_u16()) + row(origin.row_index.as_u16());
                    let picker_ops = self.branch_fuzzy.render_results(
                        state,
                        &win.branch_picker,
                        results_origin,
                        total_rows,
                        pane_width,
                        |idx, state| match state.git_refs.as_ref().and_then(|refs| refs.get(*idx)) {
                            Some(git_ref) => {
                                let accent = match git_ref.kind {
                                    RefKind::Tag => tag_rgb,
                                    _ => branch_rgb,
                                };
                                (git_ref.name.clone(), Some(accent))
                            }
                            None => (String::new(), None),
                        },
                    );
                    global_data.pipeline.push(ZOrder::Normal, picker_ops);
                }
            } else if visible == 0 {
                let message = if let Some(err) = error {
                    (format!("error: {err}"), error_fg)
                } else if entries.is_none() {
                    ("Loading git log…".to_string(), normal_fg)
                } else if filtering {
                    ("No matches".to_string(), normal_fg)
                } else {
                    ("(no commits)".to_string(), normal_fg)
                };
                let message_style = new_style!(color_fg: {message.1} color_bg: {bg});
                for row_offset in 0..total_rows {
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(0) + row(row_offset as u16),
                    );
                    render_ops += RenderOpCommon::ApplyColors(Some(bg_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".repeat(pane_width).into(),
                        Some(bg_style),
                    );
                    if row_offset == 0 {
                        render_ops +=
                            RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
                        render_ops += RenderOpCommon::ApplyColors(Some(message_style));
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            message.0.clone().into(),
                            Some(message_style),
                        );
                    }
                }
            } else {
                let history = entries.as_ref().expect("visible > 0 implies entries");
                let match_rgb = state
                    .theme
                    .ui_fg("ui.cursor.match")
                    .unwrap_or(RgbValue::from_u8(255, 200, 60));
                let match_fg = TuiColor::from(match_rgb);
                let linenum_fg_rgb = state.theme.ui_fg("ui.linenr").unwrap_or({
                    let default_fg = state
                        .theme
                        .ui_fg("ui.text")
                        .unwrap_or(RgbValue::from_u8(212, 212, 212));
                    RgbValue::from_u8(
                        default_fg.red / 3,
                        default_fg.green / 3,
                        default_fg.blue / 3,
                    )
                });
                let linenum_bg_rgb = state.theme.ui_bg("ui.linenr").unwrap_or(pane_bg);
                let selection_bg_rgb = state
                    .theme
                    .ui_bg("ui.selection")
                    .unwrap_or(DEFAULT_SELECTION_BG);
                let linenum_selected_fg_rgb = if linenum_bg_rgb == selection_bg_rgb {
                    state
                        .theme
                        .ui_bg("ui.cursor")
                        .or_else(|| state.theme.ui_fg("ui.text.focus"))
                        .or_else(|| state.theme.ui_fg("ui.text"))
                        .unwrap_or(RgbValue::from_u8(120, 120, 160))
                } else {
                    state
                        .theme
                        .ui_fg("ui.selection")
                        .unwrap_or(RgbValue::from_u8(255, 255, 255))
                };
                let linenum_normal_style = new_style!(
                    color_fg: {TuiColor::from(linenum_fg_rgb)} color_bg: {TuiColor::from(linenum_bg_rgb)}
                );
                let linenum_selected_style = new_style!(
                    color_fg: {TuiColor::from(linenum_selected_fg_rgb)} color_bg: {TuiColor::from(linenum_bg_rgb)}
                );
                let branch_fg_rgb = state
                    .theme
                    .color_for_scope("namespace")
                    .unwrap_or(RgbValue::from_u8(100, 160, 255));
                let tag_fg_rgb = match state.theme.color_for_scope("special") {
                    Some(rgb) if rgb != branch_fg_rgb => rgb,
                    _ => RgbValue::from_u8(255, 170, 80),
                };
                let branch_fg = TuiColor::from(branch_fg_rgb);
                let tag_fg = TuiColor::from(tag_fg_rgb);
                let win_filtered = state
                    .git_logs
                    .get(&window_id)
                    .and_then(|win| win.filtered.as_ref());
                for row_offset in 0..total_rows {
                    let vis_idx = scroll + row_offset;
                    let slot: Option<(usize, Option<Vec<u32>>)> = match win_filtered {
                        Some(matches) => matches
                            .get(vis_idx)
                            .map(|&(idx, ref positions)| (idx, Some(positions.to_vec()))),
                        None => history.entries.get(vis_idx).map(|_| (vis_idx, None)),
                    };
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(0) + row(row_offset as u16),
                    );
                    let is_selected = vis_idx == selected;
                    let row_style = if is_selected {
                        selected_text_style
                    } else {
                        bg_style
                    };
                    render_ops += RenderOpCommon::ApplyColors(Some(row_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".repeat(pane_width).into(),
                        Some(row_style),
                    );
                    let Some((entry_idx, match_set)) = slot else {
                        continue;
                    };
                    let Some(entry) = history.entries.get(entry_idx) else {
                        continue;
                    };
                    let (left, summary) = log_row_parts(entry, pane_width);
                    let left_chars = left.chars().count();
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(0) + row(row_offset as u16),
                    );
                    let left_style = if is_selected {
                        linenum_selected_style
                    } else {
                        linenum_normal_style
                    };
                    render_ops += RenderOpCommon::ApplyColors(Some(left_style));
                    render_ops +=
                        RenderOpIR::PaintTextWithAttributes(left.into(), Some(left_style));

                    let type_span = commit_type_prefix(&summary).map(|(ty, len)| {
                        (
                            summary.char_indices().take_while(|(b, _)| *b < len).count(),
                            TuiColor::from(commit_type_color(&state.theme, ty)),
                        )
                    });

                    for (char_i, ch) in summary.chars().enumerate() {
                        let fg = match type_span.as_ref() {
                            Some((len, color)) if char_i < *len => *color,
                            _ if match_set
                                .as_ref()
                                .is_some_and(|set| set.contains(&(char_i as u32))) =>
                            {
                                match_fg
                            }
                            _ => normal_fg,
                        };
                        let style = if is_selected {
                            new_style!(color_fg: {fg} color_bg: {selected_bg})
                        } else {
                            new_style!(color_fg: {fg} color_bg: {bg})
                        };
                        let mut buf = [0u8; 4];
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            ch.encode_utf8(&mut buf).to_string().into(),
                            Some(style),
                        );
                    }

                    if let Some(entry_refs) = history.refs.get(&entry.id) {
                        let available = pane_width
                            .saturating_sub(left_chars)
                            .saturating_sub(summary.chars().count())
                            .saturating_sub(2);
                        let (shown, ellipsis_kind) = layout_refs(available, entry_refs);
                        let names_width =
                            shown.iter().map(|r| r.name.chars().count()).sum::<usize>()
                                + shown.len().saturating_sub(1);
                        let ell_width = match ellipsis_kind {
                            Some(_) if shown.is_empty() => 3,
                            Some(_) => 4,
                            None => 0,
                        };
                        let start = pane_width.saturating_sub(names_width + ell_width);
                        render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            col(start as u16) + row(row_offset as u16),
                        );
                        for (i, git_ref) in shown.iter().enumerate() {
                            if i > 0 {
                                let sep_style = if is_selected {
                                    new_style!(color_fg: {normal_fg} color_bg: {selected_bg})
                                } else {
                                    new_style!(color_fg: {normal_fg} color_bg: {bg})
                                };
                                render_ops += RenderOpIR::PaintTextWithAttributes(
                                    " ".into(),
                                    Some(sep_style),
                                );
                            }
                            let fg = match git_ref.kind {
                                RefKind::Tag => tag_fg,
                                _ => branch_fg,
                            };
                            let style = if is_selected {
                                new_style!(color_fg: {fg} color_bg: {selected_bg})
                            } else {
                                new_style!(color_fg: {fg} color_bg: {bg})
                            };
                            render_ops += RenderOpIR::PaintTextWithAttributes(
                                git_ref.name.clone().into(),
                                Some(style),
                            );
                        }
                        if let Some(kind) = ellipsis_kind {
                            let fg = match kind {
                                RefKind::Tag => tag_fg,
                                _ => branch_fg,
                            };
                            if !shown.is_empty() {
                                let sep_style = if is_selected {
                                    new_style!(color_fg: {normal_fg} color_bg: {selected_bg})
                                } else {
                                    new_style!(color_fg: {normal_fg} color_bg: {bg})
                                };
                                render_ops += RenderOpIR::PaintTextWithAttributes(
                                    " ".into(),
                                    Some(sep_style),
                                );
                            }
                            let style = if is_selected {
                                new_style!(color_fg: {fg} color_bg: {selected_bg})
                            } else {
                                new_style!(color_fg: {fg} color_bg: {bg})
                            };
                            render_ops +=
                                RenderOpIR::PaintTextWithAttributes("...".into(), Some(style));
                        }
                    }
                }
            }

            global_data.pipeline.push(ZOrder::Normal, render_ops);
        });
    }
}

pub(crate) fn run_log_match(query: &str, entries: &[LogEntry]) -> Vec<(usize, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    if pattern.atoms.is_empty() {
        return (0..entries.len()).map(|i| (i, Vec::new())).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32, Vec<u32>)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let haystack = Utf32Str::new(&entry.summary, &mut buf);
            let mut indices = Vec::new();
            pattern
                .indices(haystack, &mut matcher, &mut indices)
                .map(|score| {
                    indices.sort_unstable();
                    indices.dedup();
                    (i, score, indices)
                })
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    scored
        .into_iter()
        .map(|(i, _, indices)| (i, indices))
        .collect()
}

pub(crate) fn run_ref_match(query: &str, refs: &[GitRef]) -> Vec<(usize, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    if pattern.atoms.is_empty() {
        return (0..refs.len()).map(|i| (i, Vec::new())).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32, Vec<u32>)> = refs
        .iter()
        .enumerate()
        .filter_map(|(i, git_ref)| {
            let haystack = Utf32Str::new(&git_ref.name, &mut buf);
            let mut indices = Vec::new();
            pattern
                .indices(haystack, &mut matcher, &mut indices)
                .map(|score| {
                    indices.sort_unstable();
                    indices.dedup();
                    (i, score, indices)
                })
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    scored
        .into_iter()
        .map(|(i, _, indices)| (i, indices))
        .collect()
}

pub(crate) fn apply_filter(win: &mut GitLogWindowState) {
    let total = win
        .entries
        .as_ref()
        .map_or(0, |history| history.entries.len());
    if win.query.is_empty() {
        win.filtered = None;
    } else {
        let matches = win.entries.as_ref().map_or(Vec::new(), |history| {
            run_log_match(&win.query, &history.entries)
        });
        win.filtered = Some(matches);
    }
    let visible = win.filtered.as_ref().map_or(total, |matches| matches.len());
    if win.selected >= visible {
        win.selected = visible.saturating_sub(1);
    }
}

fn visible_len(win: &GitLogWindowState) -> usize {
    let total = win
        .entries
        .as_ref()
        .map_or(0, |history| history.entries.len());
    win.filtered.as_ref().map_or(total, |matches| matches.len())
}

fn visible_entry_id(win: &GitLogWindowState) -> Option<[u8; 20]> {
    let history = win.entries.as_ref()?;
    let entry_idx = match win.filtered.as_ref() {
        Some(matches) => matches.get(win.selected).map(|&(idx, _)| idx)?,
        None => {
            if win.selected < history.entries.len() {
                win.selected
            } else {
                return None;
            }
        }
    };
    history.entries.get(entry_idx).map(|entry| entry.id)
}

fn commit_type_color(theme: &HelixTheme, ty: &str) -> RgbValue {
    let scope = match ty {
        "feat" => "diff.plus",
        "fix" => "diff.minus",
        "perf" => "diff.delta",
        "docs" => "comment",
        "test" => "special",
        "refactor" => "keyword",
        "style" => "attribute",
        "build" => "type",
        "ci" => "constant",
        "chore" | "revert" => "operator",
        _ => "ui.text",
    };
    theme.color_for_scope(scope).unwrap_or_else(|| {
        theme
            .ui_fg("ui.text")
            .unwrap_or(RgbValue::from_u8(170, 170, 200))
    })
}

fn log_row_parts(entry: &LogEntry, pane_width: usize) -> (String, String) {
    let date = format_log_time(entry.time_secs, entry.time_offset_secs);
    let gap = "   ";
    let max_chars = pane_width.saturating_sub(1);
    let summary_max = max_chars.saturating_sub(date.chars().count() + gap.len());
    let summary: String = entry.summary.chars().take(summary_max).collect();
    (format!("{date}{gap}"), summary)
}

fn layout_refs(available: usize, refs: &[GitRef]) -> (Vec<&GitRef>, Option<RefKind>) {
    if available == 0 || refs.is_empty() {
        return (Vec::new(), None);
    }
    let mut shown: Vec<&GitRef> = refs.iter().collect();
    let mut ellipsis_kind: Option<RefKind> = None;
    loop {
        let mut width = shown.iter().map(|r| r.name.chars().count()).sum::<usize>()
            + shown.len().saturating_sub(1);
        if ellipsis_kind.is_some() {
            width += if shown.is_empty() { 3 } else { 4 };
        }
        if width <= available {
            return (shown, ellipsis_kind);
        }
        match shown.pop() {
            Some(dropped) => {
                if ellipsis_kind.is_none() {
                    ellipsis_kind = Some(dropped.kind);
                }
            }
            None => return (Vec::new(), None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_with_summary(summary: &str) -> LogEntry {
        LogEntry {
            id: [0u8; 20],
            time_secs: 0,
            time_offset_secs: 0,
            summary: summary.to_string(),
        }
    }

    fn git_ref(kind: RefKind, name: &str) -> GitRef {
        GitRef {
            kind,
            name: name.to_string(),
        }
    }

    #[test]
    fn layout_refs_shows_all_when_they_fit() {
        let refs = [
            git_ref(RefKind::Branch, "main"),
            git_ref(RefKind::Tag, "v1.0"),
        ];
        let (shown, ellipsis) = layout_refs(40, &refs);
        assert_eq!(shown.len(), 2);
        assert_eq!(ellipsis, None);
    }

    #[test]
    fn layout_refs_drops_tags_first_with_tag_ellipsis() {
        let refs = [
            git_ref(RefKind::Branch, "main"),
            git_ref(RefKind::Tag, "v1.0"),
            git_ref(RefKind::Tag, "v0.9"),
        ];
        let (shown, ellipsis) = layout_refs(10, &refs);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].name, "main");
        assert_eq!(ellipsis, Some(RefKind::Tag));
    }

    #[test]
    fn layout_refs_drops_last_branch_with_branch_ellipsis() {
        let refs = [git_ref(RefKind::Branch, "feature-x")];
        let (shown, ellipsis) = layout_refs(6, &refs);
        assert!(shown.is_empty());
        assert_eq!(ellipsis, Some(RefKind::Branch));
    }

    #[test]
    fn layout_refs_shows_nothing_when_ellipsis_does_not_fit() {
        let refs = [git_ref(RefKind::Branch, "main")];
        let (shown, ellipsis) = layout_refs(2, &refs);
        assert!(shown.is_empty());
        assert_eq!(ellipsis, None);
    }

    #[test]
    fn layout_refs_handles_narrow_available_width() {
        let refs = [
            git_ref(RefKind::Branch, "main"),
            git_ref(RefKind::Tag, "v1"),
        ];
        let (shown, ellipsis) = layout_refs(0, &refs);
        assert!(shown.is_empty());
        assert_eq!(ellipsis, None);
    }

    #[test]
    fn log_row_parts_separates_date_with_three_spaces() {
        let entry = entry_with_summary("feat: x");
        let (left, summary) = log_row_parts(&entry, 60);
        assert_eq!(left, "1970-01-01 00:00   ");
        assert_eq!(summary, "feat: x");
    }

    #[test]
    fn log_row_parts_truncates_summary_on_char_boundaries() {
        let entry = entry_with_summary("éééééééééé éééééééééé");
        let (_, summary) = log_row_parts(&entry, 40);
        assert!(summary.chars().count() <= 40 - 1 - 19);
        assert_eq!(
            summary,
            entry.summary.chars().take(40 - 1 - 19).collect::<String>()
        );
    }

    #[test]
    fn run_log_match_empty_query_lists_everything_in_order() {
        let entries = vec![
            entry_with_summary("feat: one"),
            entry_with_summary("fix: two"),
        ];
        let results = run_log_match("", &entries);
        assert_eq!(results, vec![(0, vec![]), (1, vec![])]);
    }

    #[test]
    fn run_log_match_ranks_and_records_positions() {
        let entries = vec![
            entry_with_summary("docs: update readme"),
            entry_with_summary("feat: readme reader"),
        ];
        let results = run_log_match("readme", &entries);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|&(idx, _)| idx == 0 || idx == 1));
        let (_, positions) = &results[0];
        assert!(!positions.is_empty());
        let summary = entries[results[0].0].summary.clone();
        assert!(
            positions
                .iter()
                .all(|&p| { summary.chars().nth(p as usize).is_some() })
        );
    }
}
