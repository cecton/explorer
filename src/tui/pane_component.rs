use crate::tui::*;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;
use tokio::sync::mpsc;

/// Dispatcher component for a single pane slot. Holds both inner component types and
/// delegates to the correct one based on which `Window` is currently assigned to this slot
/// by `state.pane_manager.layout(state.last_surface_size)`.
pub struct PaneComponent {
    pub id: FlexBoxId,
    pub slot: usize,
    picker: FileNamePickerComponent,
    theme_picker: ThemePickerComponent,
    preview: FilePreviewComponent,
    terminal: TerminalPaneComponent,
    /// Origin row of the content area (below title bar), used for scrollbar mouse events.
    content_origin_row: u16,
    /// Total columns in the content area (full width including scrollbar column).
    content_col_count: u16,
    /// Total rows in the content area.
    content_row_count: u16,
    /// Origin column of the content area, used for absolute scrollbar column calculation.
    content_origin_col: u16,
    /// Whether the user is currently dragging the scrollbar thumb.
    scrollbar_dragging: bool,
    /// (scroll, rel_y) at thumb grab time (None if drag started on track).
    scrollbar_grab_state: Option<(usize, usize)>,
    preview_drag_active: bool,
    text_drag_active: bool,
    last_click: Option<(Instant, Pos)>,
    consecutive_clicks: u8,
}

impl PaneComponent {
    pub fn new_boxed(
        slot: usize,
        id: FlexBoxId,
        picker_results_tx: mpsc::Sender<PickerResultMsg>,
        picker_generation: Arc<AtomicU64>,
    ) -> BoxedSafeComponent<AppState, AppSignal> {
        Box::new(Self {
            id,
            slot,
            picker: FileNamePickerComponent::new(id, picker_results_tx, picker_generation),
            theme_picker: ThemePickerComponent::new(id),
            preview: FilePreviewComponent::new(id),
            terminal: TerminalPaneComponent::new(id, slot),
            content_origin_row: 0,
            content_col_count: 0,
            content_row_count: 0,
            content_origin_col: 0,
            scrollbar_dragging: false,
            scrollbar_grab_state: None,
            preview_drag_active: false,
            text_drag_active: false,
            last_click: None,
            consecutive_clicks: 0,
        })
    }

    fn active_window(&self, state: &AppState) -> Option<Window> {
        state
            .pane_manager
            .layout(state.last_surface_size)
            .into_iter()
            .find(|slot| slot.slot == self.slot)
            .map(|slot| slot.window)
    }

    fn handle_scrollbar(
        &mut self,
        mouse: MouseInput,
        global_data: &mut GlobalData<AppState, AppSignal>,
        window: &Window,
    ) -> EventPropagation {
        let row = mouse.pos.row_index.as_usize();
        let rel_y = row.saturating_sub(self.content_origin_row as usize);

        let state = &mut global_data.state;
        let (scroll, scroll_max, page_size) = match window {
            Window::Terminal(id) => {
                let Some(pane) = state.terminal_panes.get(id) else {
                    return EventPropagation::Propagate;
                };
                let Ok(pane) = pane.lock() else {
                    return EventPropagation::Propagate;
                };
                if pane.ofs_buf.terminal_mode.active_screen_buffer == ActiveScreenBuffer::Alternate
                {
                    return EventPropagation::Propagate;
                }
                let scrollback_len = pane.ofs_buf.scrollback_len();
                let buffer_height = pane.ofs_buf.get_window_size().row_height.as_usize();
                if scrollback_len == 0 {
                    return EventPropagation::Propagate;
                }
                let scroll = scrollback_len.saturating_sub(pane.scroll_offset.min(scrollback_len));
                (scroll, scrollback_len + buffer_height, buffer_height)
            }
            _ => {
                let scroll = state.pane_manager.window_scroll(window);
                let scroll_max = state.pane_manager.window_scroll_max(window);
                let page_size = state.pane_manager.window_page_size(window);
                (scroll, scroll_max, page_size)
            }
        };
        let scrollbar_height = self.content_row_count as usize;

        match mouse.kind {
            MouseInputKind::MouseDown(Button::Left) => {
                if scroll_max > page_size && scrollbar_height > 0 {
                    let thumb_height = thumb_size(scrollbar_height, page_size, scroll_max);
                    let thumb_pos = thumb_position(
                        scroll,
                        scrollbar_height,
                        thumb_height,
                        scroll_max,
                        page_size,
                    );
                    if rel_y >= thumb_pos && rel_y < thumb_pos + thumb_height {
                        // Grab the thumb: snapshot current state, no scroll jump.
                        self.scrollbar_dragging = true;
                        self.scrollbar_grab_state = Some((scroll, rel_y));
                        return EventPropagation::ConsumedRender;
                    } else {
                        // Click on track: jump directly to clicked position.
                        self.scrollbar_dragging = true;
                        self.scrollbar_grab_state = None;
                        let target = scroll_from_y(rel_y, scrollbar_height, scroll_max, page_size);
                        return self.apply_scroll(state, window, target);
                    }
                }
                EventPropagation::ConsumedRender
            }
            MouseInputKind::MouseDrag(Button::Left) if self.scrollbar_dragging => {
                if scroll_max > page_size && scrollbar_height > 0 {
                    let target = if let Some((grab_scroll, grab_rel_y)) = self.scrollbar_grab_state
                    {
                        let thumb_height = thumb_size(scrollbar_height, page_size, scroll_max);
                        let denom = (scrollbar_height - thumb_height).max(1);
                        let range = scroll_max - page_size;
                        let delta_y = (rel_y as isize) - (grab_rel_y as isize);
                        let delta_scroll = delta_y * (range as isize) / (denom as isize);
                        ((grab_scroll as isize) + delta_scroll).max(0) as usize
                    } else {
                        scroll_from_y(rel_y, scrollbar_height, scroll_max, page_size)
                    };
                    return self.apply_scroll(state, window, target);
                }
                EventPropagation::ConsumedRender
            }
            MouseInputKind::MouseUp(Button::Left) => {
                self.scrollbar_dragging = false;
                self.scrollbar_grab_state = None;
                EventPropagation::ConsumedRender
            }
            MouseInputKind::ScrollUp => {
                let target = scroll.saturating_sub(3);
                self.apply_scroll(state, window, target)
            }
            MouseInputKind::ScrollDown => {
                let target = scroll.saturating_add(3);
                self.apply_scroll(state, window, target)
            }
            _ => EventPropagation::ConsumedRender,
        }
    }

    fn apply_scroll(
        &mut self,
        state: &mut AppState,
        window: &Window,
        target: usize,
    ) -> EventPropagation {
        match window {
            Window::FilePreview(_) => {
                state.pane_manager.set_window_scroll(window, target);
                state.pane_manager.clamp_scroll(window);
                state.mark_session_dirty();
                EventPropagation::ConsumedRender
            }
            Window::FileNamePicker => {
                let scroll_max = state.pane_manager.window_scroll_max(window);
                if scroll_max == 0 {
                    return EventPropagation::ConsumedRender;
                }
                let idx = target.min(scroll_max.saturating_sub(1));
                if let Some((key, _)) = state.file_name_picker.results.get(idx) {
                    state.file_name_picker.selected = Some(*key);
                }
                EventPropagation::ConsumedRender
            }
            Window::ThemePicker => {
                let scroll_max = state.pane_manager.window_scroll_max(window);
                if scroll_max == 0 {
                    return EventPropagation::ConsumedRender;
                }
                let idx = target.min(scroll_max.saturating_sub(1));
                if let Some((name, _)) = state.theme_picker.results.get(idx) {
                    state.theme_picker.selected = Some(name.clone());
                    if let Some(theme) = HelixTheme::from_name(name) {
                        state.theme = theme;
                    }
                }
                EventPropagation::ConsumedRender
            }
            Window::Terminal(id) => {
                let Some(pane) = state.terminal_panes.get(id) else {
                    return EventPropagation::ConsumedRender;
                };
                if let Ok(mut pane) = pane.lock() {
                    let scrollback_len = pane.ofs_buf.scrollback_len();
                    let clamped = target.min(scrollback_len);
                    pane.scroll_offset = scrollback_len.saturating_sub(clamped);
                }
                state.terminal_grabbed = false;
                EventPropagation::ConsumedRender
            }
        }
    }
}

impl TitleRow for PaneComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
    ) -> usize {
        let Some(window) = self.active_window(state) else {
            return 0;
        };
        match window {
            Window::FileNamePicker => self
                .picker
                .render_title_row(ops, pane_box, focused, theme, state),
            Window::ThemePicker => self
                .theme_picker
                .render_title_row(ops, pane_box, focused, theme, state),
            Window::FilePreview(_) => self
                .preview
                .render_title_row(ops, pane_box, focused, theme, state),
            Window::Terminal(_) => self
                .terminal
                .render_title_row(ops, pane_box, focused, theme, state),
        }
    }
}

fn thumb_size(scrollbar_height: usize, page_size: usize, scroll_max: usize) -> usize {
    std::cmp::max(1, (scrollbar_height * page_size) / scroll_max.max(1))
}

fn thumb_position(
    scroll: usize,
    scrollbar_height: usize,
    thumb_height: usize,
    scroll_max: usize,
    page_size: usize,
) -> usize {
    if scroll_max <= page_size {
        0
    } else {
        (scroll * (scrollbar_height - thumb_height)) / (scroll_max - page_size)
    }
}

fn scroll_from_y(
    rel_y: usize,
    scrollbar_height: usize,
    scroll_max: usize,
    page_size: usize,
) -> usize {
    if scroll_max <= page_size || scrollbar_height == 0 {
        return 0;
    }
    (rel_y * (scroll_max - page_size)) / (scrollbar_height.saturating_sub(1).max(1))
}

fn drag_modifier_from_mouse(mouse: &MouseInput) -> Option<DragModifier> {
    let mask = mouse.maybe_modifier_keys?;
    let shift = mask.shift_key_state == KeyState::Pressed;
    let ctrl = mask.ctrl_key_state == KeyState::Pressed;
    let alt = mask.alt_key_state == KeyState::Pressed;
    if alt {
        return None;
    }
    match (shift, ctrl) {
        (true, false) => Some(DragModifier::Shift),
        (false, true) => Some(DragModifier::Ctrl),
        _ => None,
    }
}

impl Component<AppState, AppSignal> for PaneComponent {
    fn reset(&mut self) {
        self.picker.reset();
        self.theme_picker.reset();
        self.preview.reset();
        self.terminal.reset();
        self.preview_drag_active = false;
        self.text_drag_active = false;
        self.last_click = None;
        self.consecutive_clicks = 0;
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        input_event: InputEvent,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        let active_window = self.active_window(&global_data.state);

        // If a preview drag was active but the window changed, clean up.
        if self.preview_drag_active
            && !matches!(active_window.as_ref(), Some(Window::FilePreview(_)))
        {
            self.preview_drag_active = false;
            self.preview.end_drag();
            global_data.state.mouse_drag_active = false;
        }

        if self.text_drag_active
            && !matches!(
                active_window.as_ref(),
                Some(Window::FilePreview(_)) | Some(Window::Terminal(_))
            )
        {
            self.text_drag_active = false;
            global_data.state.text_selection = None;
            global_data.state.mouse_drag_active = false;
            self.last_click = None;
            self.consecutive_clicks = 0;
        }

        // Check for scrollbar mouse interaction first.
        if !self.preview_drag_active
            && !self.text_drag_active
            && let InputEvent::Mouse(mouse) = input_event
        {
            let col = mouse.pos.col_index.as_usize();
            let row = mouse.pos.row_index.as_usize();
            let origin_col = self.content_origin_col as usize;
            let origin_row = self.content_origin_row as usize;
            let col_count = self.content_col_count as usize;
            let scrollbar_col = origin_col + col_count.saturating_sub(1);
            let bottom_row = origin_row + self.content_row_count as usize;

            let in_vertical = self.scrollbar_dragging || (row >= origin_row && row < bottom_row);
            let in_horizontal = col == scrollbar_col
                || (self.scrollbar_dragging && col >= origin_col && col < origin_col + col_count);

            if self.content_row_count > 0
                && in_vertical
                && in_horizontal
                && let Some(ref window) = active_window
            {
                return Ok(self.handle_scrollbar(mouse, global_data, window));
            }
        }

        // Check for title bar range click.
        if let InputEvent::Mouse(mouse) = input_event
            && let Some(window) = active_window
            && let Window::FilePreview(key) = window
        {
            let title_bar_row = self.content_origin_row.saturating_sub(1) as usize;
            let origin_col = self.content_origin_col as usize;
            let pane_width = self.content_col_count as usize;
            let col = mouse.pos.col_index.as_usize();
            let row = mouse.pos.row_index.as_usize();

            if row == title_bar_row
                && col >= origin_col
                && col < origin_col + pane_width
                && matches!(mouse.kind, MouseInputKind::MouseDown(Button::Left))
                && let Some((lo, hi)) = self.preview.range_at_title_col(
                    &global_data.state,
                    col - origin_col,
                    pane_width,
                )
            {
                self.preview
                    .scroll_to_range(&mut global_data.state, key, lo, hi);
                global_data.state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }
        }

        // Preview content drag.
        if let InputEvent::Mouse(mouse) = input_event
            && let Some(window) = active_window
            && let Window::FilePreview(key) = window
        {
            let col = mouse.pos.col_index.as_usize();
            let row = mouse.pos.row_index.as_usize();
            let origin_row = self.content_origin_row as usize;
            let origin_col = self.content_origin_col as usize;
            let col_count = self.content_col_count as usize;
            let row_count = self.content_row_count as usize;

            let in_content_rows = row >= origin_row && row < origin_row + row_count;
            let in_content_cols =
                col >= origin_col && col < origin_col + col_count.saturating_sub(1);

            match mouse.kind {
                MouseInputKind::MouseDown(Button::Left) if in_content_rows && in_content_cols => {
                    if let Some(modifier) = drag_modifier_from_mouse(&mouse) {
                        let state = &mut global_data.state;
                        let window = Window::FilePreview(key);
                        let scroll = state.pane_manager.window_scroll(&window);
                        let scroll_max = state.pane_manager.window_scroll_max(&window);
                        let rel_y = row.saturating_sub(origin_row);
                        let line = (scroll + rel_y + 1).clamp(1, scroll_max.max(1));

                        self.preview_drag_active = true;
                        self.preview.start_drag(state, key, line, modifier);
                        state.mouse_drag_active = true;
                        return Ok(EventPropagation::ConsumedRender);
                    } else {
                        let state = &mut global_data.state;
                        let now = Instant::now();
                        let is_same_pos = self.last_click.is_some_and(|(_, p)| p == mouse.pos);
                        let is_quick = self
                            .last_click
                            .is_some_and(|(t, _)| now.duration_since(t).as_millis() < 300);
                        if is_same_pos && is_quick {
                            self.consecutive_clicks = self.consecutive_clicks.saturating_add(1);
                        } else {
                            self.consecutive_clicks = 1;
                        }
                        self.last_click = Some((now, mouse.pos));

                        let files = Arc::clone(&state.files);
                        let snapshot = files.load();
                        let file = &snapshot[key.0];
                        if let Ok(data) = file.data.lock() {
                            let (line_idx, _char_idx, cursor_byte) = self
                                .preview
                                .screen_pos_to_line_char(state, row, col, key, &data);

                            match self.consecutive_clicks {
                                1 => {
                                    let bounds = data.word_bounds(cursor_byte);
                                    state.text_selection = Some(TextSelection {
                                        window: Window::FilePreview(key),
                                        start: SelPoint::Preview {
                                            line_idx,
                                            byte_offset: bounds.0,
                                        },
                                        end: SelPoint::Preview {
                                            line_idx,
                                            byte_offset: bounds.0,
                                        },
                                        click_anchor: Some(SelPoint::Preview {
                                            line_idx,
                                            byte_offset: cursor_byte,
                                        }),
                                        click_word: Some((
                                            SelPoint::Preview {
                                                line_idx,
                                                byte_offset: bounds.0,
                                            },
                                            SelPoint::Preview {
                                                line_idx,
                                                byte_offset: bounds.1,
                                            },
                                        )),
                                        active: true,
                                    });
                                    self.text_drag_active = true;
                                    state.mouse_drag_active = true;
                                }
                                3 => {
                                    let bounds = data.line_bounds(line_idx);
                                    state.text_selection = Some(TextSelection {
                                        window: Window::FilePreview(key),
                                        start: SelPoint::Preview {
                                            line_idx,
                                            byte_offset: bounds.0,
                                        },
                                        end: SelPoint::Preview {
                                            line_idx,
                                            byte_offset: bounds.1,
                                        },
                                        click_anchor: None,
                                        click_word: None,
                                        active: true,
                                    });
                                    self.text_drag_active = true;
                                    state.mouse_drag_active = true;
                                }
                                _ => {}
                            }
                            return Ok(EventPropagation::ConsumedRender);
                        } else {
                            return Ok(EventPropagation::ConsumedRender);
                        }
                    }
                }
                MouseInputKind::MouseDrag(Button::Left) if self.text_drag_active => {
                    let state = &mut global_data.state;
                    let files = Arc::clone(&state.files);
                    let snapshot = files.load();
                    let file = &snapshot[key.0];
                    if let Ok(data) = file.data.lock() {
                        let (line_idx, _char_idx, cursor_byte) = self
                            .preview
                            .screen_pos_to_line_char(state, row, col, key, &data);

                        let Some(ref mut sel) = state.text_selection else {
                            return Ok(EventPropagation::ConsumedRender);
                        };
                        if sel.window != Window::FilePreview(key) {
                            return Ok(EventPropagation::ConsumedRender);
                        }

                        if let (
                            Some(SelPoint::Preview {
                                line_idx: anchor_line,
                                byte_offset: anchor_byte,
                            }),
                            Some((
                                SelPoint::Preview {
                                    byte_offset: word_start,
                                    ..
                                },
                                SelPoint::Preview {
                                    byte_offset: word_end,
                                    ..
                                },
                            )),
                        ) = (sel.click_anchor, sel.click_word)
                            && anchor_line == line_idx
                        {
                            if cursor_byte >= anchor_byte {
                                sel.start = SelPoint::Preview {
                                    line_idx,
                                    byte_offset: word_start,
                                };
                                let cur = data.word_bounds(cursor_byte);
                                sel.end = SelPoint::Preview {
                                    line_idx,
                                    byte_offset: cur.1,
                                };
                            } else {
                                let cur = data.word_bounds(cursor_byte);
                                sel.start = SelPoint::Preview {
                                    line_idx,
                                    byte_offset: cur.0,
                                };
                                sel.end = SelPoint::Preview {
                                    line_idx,
                                    byte_offset: word_end,
                                };
                            }
                            return Ok(EventPropagation::ConsumedRender);
                        }

                        sel.end = SelPoint::Preview {
                            line_idx,
                            byte_offset: cursor_byte,
                        };
                        return Ok(EventPropagation::ConsumedRender);
                    } else {
                        return Ok(EventPropagation::ConsumedRender);
                    }
                }
                MouseInputKind::MouseDrag(Button::Left) if self.preview_drag_active => {
                    let state = &mut global_data.state;
                    let window = Window::FilePreview(key);
                    let scroll = state.pane_manager.window_scroll(&window);
                    let scroll_max = state.pane_manager.window_scroll_max(&window);
                    let rel_y = row.saturating_sub(origin_row);
                    let line = (scroll + rel_y + 1).clamp(1, scroll_max.max(1));

                    let before = state.highlight_ranges.get(&key).cloned();
                    self.preview.update_drag(state, key, line);
                    let after = state.highlight_ranges.get(&key).cloned();
                    if before != after {
                        state.mark_session_dirty();
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                MouseInputKind::MouseUp(Button::Left) if self.text_drag_active => {
                    let state = &mut global_data.state;
                    if let Some(ref sel) = state.text_selection
                        && sel.window == Window::FilePreview(key)
                    {
                        let snapshot = state.files.load();
                        let file = &snapshot[key.0];
                        if let Ok(data) = file.data.lock() {
                            let (start_byte, end_byte) = match (sel.start, sel.end) {
                                (
                                    SelPoint::Preview {
                                        line_idx: s_line,
                                        byte_offset: s_byte,
                                    },
                                    SelPoint::Preview {
                                        line_idx: e_line,
                                        byte_offset: e_byte,
                                    },
                                ) => {
                                    if s_line == e_line {
                                        let (lo, hi) = (s_byte.min(e_byte), s_byte.max(e_byte));
                                        (lo, hi)
                                    } else {
                                        let s_bounds = data.line_bounds(s_line.min(e_line));
                                        let e_bounds = data.line_bounds(s_line.max(e_line));
                                        (s_bounds.0, e_bounds.1)
                                    }
                                }
                                _ => (0, 0),
                            };
                            if let Some(text) = data.extract_text(start_byte, end_byte) {
                                let mut cb = r3bl_tui::SystemClipboard;
                                let _ = cb.try_to_put_content_into_clipboard(text);
                            }
                        }
                    }
                    state.text_selection = None;
                    self.text_drag_active = false;
                    state.mouse_drag_active = false;
                    return Ok(EventPropagation::ConsumedRender);
                }
                MouseInputKind::MouseUp(Button::Left) if self.preview_drag_active => {
                    self.preview_drag_active = false;
                    self.preview.end_drag();
                    global_data.state.mouse_drag_active = false;
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        // Terminal content selection.
        if let InputEvent::Mouse(mouse) = input_event
            && let Some(window) = active_window
            && let Window::Terminal(term_id) = window
        {
            let col = mouse.pos.col_index.as_usize();
            let row = mouse.pos.row_index.as_usize();
            let origin_row = self.content_origin_row as usize;
            let origin_col = self.content_origin_col as usize;
            let col_count = self.content_col_count as usize;
            let row_count = self.content_row_count as usize;

            let in_content_rows = row >= origin_row && row < origin_row + row_count;
            let in_content_cols = col >= origin_col && col < origin_col + col_count;

            let (is_alternate, mouse_tracking_active) = global_data
                .state
                .terminal_panes
                .get(&term_id)
                .and_then(|p| p.lock().ok())
                .map(|p| {
                    let alt = p.ofs_buf.terminal_mode.active_screen_buffer
                        == ActiveScreenBuffer::Alternate;
                    let mouse =
                        p.ofs_buf.terminal_mode.mouse_tracking_mode != MouseTrackingMode::Disabled;
                    (alt, mouse)
                })
                .unwrap_or((false, false));

            let selection_blocked = is_alternate && mouse_tracking_active;

            match mouse.kind {
                MouseInputKind::MouseDown(Button::Left)
                    if in_content_rows && in_content_cols && !selection_blocked =>
                {
                    let rel_row = row - origin_row;
                    let rel_col = col - origin_col;

                    let word_bounds = global_data
                        .state
                        .terminal_panes
                        .get(&term_id)
                        .and_then(|p| p.lock().ok())
                        .map(|pane| {
                            let (line, _) =
                                terminal_line_at_viewport_row(&pane, rel_row, row_count);
                            let (ws, we) = terminal_word_bounds(&line, rel_col);
                            (ws, we)
                        });

                    let state = &mut global_data.state;
                    if let Some((ws, we)) = word_bounds {
                        state.text_selection = Some(TextSelection {
                            window: Window::Terminal(term_id),
                            start: SelPoint::Terminal {
                                viewport_row: rel_row,
                                col: ws,
                            },
                            end: SelPoint::Terminal {
                                viewport_row: rel_row,
                                col: we,
                            },
                            click_anchor: Some(SelPoint::Terminal {
                                viewport_row: rel_row,
                                col: rel_col,
                            }),
                            click_word: Some((
                                SelPoint::Terminal {
                                    viewport_row: rel_row,
                                    col: ws,
                                },
                                SelPoint::Terminal {
                                    viewport_row: rel_row,
                                    col: we,
                                },
                            )),
                            active: true,
                        });
                    } else {
                        state.text_selection = Some(TextSelection {
                            window: Window::Terminal(term_id),
                            start: SelPoint::Terminal {
                                viewport_row: rel_row,
                                col: rel_col,
                            },
                            end: SelPoint::Terminal {
                                viewport_row: rel_row,
                                col: rel_col,
                            },
                            click_anchor: None,
                            click_word: None,
                            active: true,
                        });
                    }
                    self.text_drag_active = true;
                    state.mouse_drag_active = true;
                    return Ok(EventPropagation::ConsumedRender);
                }
                MouseInputKind::MouseDrag(Button::Left)
                    if self.text_drag_active && !selection_blocked =>
                {
                    let rel_row = row
                        .saturating_sub(origin_row)
                        .min(row_count.saturating_sub(1));
                    let rel_col = col
                        .saturating_sub(origin_col)
                        .min(col_count.saturating_sub(1));

                    let click_anchor_row =
                        global_data.state.text_selection.as_ref().and_then(|sel| {
                            match sel.click_anchor {
                                Some(SelPoint::Terminal { viewport_row, .. }) => Some(viewport_row),
                                _ => None,
                            }
                        });

                    let (word_bounds, cur_line_len) = global_data
                        .state
                        .terminal_panes
                        .get(&term_id)
                        .and_then(|p| p.lock().ok())
                        .map(|pane| {
                            let (line, count) =
                                terminal_line_at_viewport_row(&pane, rel_row, row_count);
                            let wb = terminal_word_bounds(&line, rel_col);
                            let cl = click_anchor_row.filter(|&ar| ar != rel_row).map(|_| count);
                            (Some(wb), cl)
                        })
                        .unwrap_or((None, None));

                    let state = &mut global_data.state;
                    let Some(ref mut sel) = state.text_selection else {
                        return Ok(EventPropagation::ConsumedRender);
                    };
                    if sel.window != Window::Terminal(term_id) {
                        return Ok(EventPropagation::ConsumedRender);
                    }

                    if let (
                        Some(SelPoint::Terminal {
                            viewport_row: anchor_row,
                            col: anchor_col,
                        }),
                        Some((
                            SelPoint::Terminal {
                                col: word_start, ..
                            },
                            SelPoint::Terminal { col: word_end, .. },
                        )),
                    ) = (sel.click_anchor, sel.click_word)
                        && anchor_row == rel_row
                        && let Some((cur_start, cur_end)) = word_bounds
                    {
                        if rel_col >= anchor_col {
                            sel.start = SelPoint::Terminal {
                                viewport_row: rel_row,
                                col: word_start,
                            };
                            sel.end = SelPoint::Terminal {
                                viewport_row: rel_row,
                                col: cur_end,
                            };
                        } else {
                            sel.start = SelPoint::Terminal {
                                viewport_row: rel_row,
                                col: cur_start,
                            };
                            sel.end = SelPoint::Terminal {
                                viewport_row: rel_row,
                                col: word_end,
                            };
                        }
                        return Ok(EventPropagation::ConsumedRender);
                    }

                    if let Some(anchor_row) = click_anchor_row
                        && anchor_row != rel_row
                        && let Some(cur_len) = cur_line_len
                    {
                        sel.start = SelPoint::Terminal {
                            viewport_row: anchor_row,
                            col: 0,
                        };
                        sel.end = SelPoint::Terminal {
                            viewport_row: rel_row,
                            col: cur_len,
                        };
                        return Ok(EventPropagation::ConsumedRender);
                    }

                    sel.end = SelPoint::Terminal {
                        viewport_row: rel_row,
                        col: rel_col,
                    };
                    return Ok(EventPropagation::ConsumedRender);
                }
                MouseInputKind::MouseUp(Button::Left)
                    if self.text_drag_active && !selection_blocked =>
                {
                    let maybe_text = if let Some(ref sel) = global_data.state.text_selection {
                        if sel.window == Window::Terminal(term_id) {
                            global_data
                                .state
                                .terminal_panes
                                .get(&term_id)
                                .and_then(|p| p.lock().ok())
                                .and_then(|pane| {
                                    extract_terminal_text(&pane, sel.start, sel.end, row_count)
                                        .filter(|t| !t.is_empty())
                                })
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(text) = maybe_text {
                        let mut cb = r3bl_tui::SystemClipboard;
                        let _ = cb.try_to_put_content_into_clipboard(text);
                    }
                    let state = &mut global_data.state;
                    state.text_selection = None;
                    self.text_drag_active = false;
                    state.mouse_drag_active = false;
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        match active_window {
            Some(Window::FileNamePicker) => {
                self.picker
                    .handle_event(global_data, input_event, has_focus)
            }
            Some(Window::ThemePicker) => {
                self.theme_picker
                    .handle_event(global_data, input_event, has_focus)
            }
            Some(Window::FilePreview(_)) => {
                self.preview
                    .handle_event(global_data, input_event, has_focus)
            }
            Some(Window::Terminal(_)) => {
                self.terminal
                    .handle_event(global_data, input_event, has_focus)
            }
            None => Ok(EventPropagation::Propagate),
        }
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        surface_bounds: SurfaceBounds,
        has_focus: &mut HasFocus,
    ) -> CommonResult {
        throws!({
            let mut title_ops = RenderOpIRVec::new();
            let focused = has_focus.get_id() == Some(self.id);
            let active_window = self.active_window(&global_data.state);
            let title_height = self.render_title_row(
                &mut title_ops,
                &current_box,
                focused,
                &global_data.state.theme,
                &global_data.state,
            );

            let has_scrollbar = match active_window {
                Some(Window::Terminal(id)) => global_data
                    .state
                    .terminal_panes
                    .get(&id)
                    .and_then(|p| p.lock().ok())
                    .is_none_or(|pane| {
                        pane.ofs_buf.terminal_mode.active_screen_buffer
                            != ActiveScreenBuffer::Alternate
                    }),
                _ => true,
            };

            let (content_box, inner_bounds) = if title_height > 0 {
                let origin = current_box.style_adjusted_origin_pos + height(title_height as u16);
                let bounds = current_box.style_adjusted_bounds_size.col_width
                    + (current_box.style_adjusted_bounds_size.row_height
                        - height(title_height as u16));
                let scrollbar_col = if has_scrollbar {
                    bounds.col_width - width(1)
                } else {
                    bounds.col_width
                };
                let inner_bounds = scrollbar_col + bounds.row_height;
                let boxed = FlexBox {
                    style_adjusted_origin_pos: origin,
                    style_adjusted_bounds_size: bounds,
                    ..current_box
                };
                (
                    boxed,
                    FlexBox {
                        style_adjusted_origin_pos: origin,
                        style_adjusted_bounds_size: inner_bounds,
                        ..current_box
                    },
                )
            } else {
                let bounds = current_box.style_adjusted_bounds_size;
                let scrollbar_col = if has_scrollbar {
                    bounds.col_width - width(1)
                } else {
                    bounds.col_width
                };
                let inner_bounds = scrollbar_col + bounds.row_height;
                let boxed = FlexBox {
                    style_adjusted_bounds_size: bounds,
                    ..current_box
                };
                (
                    boxed,
                    FlexBox {
                        style_adjusted_bounds_size: inner_bounds,
                        ..current_box
                    },
                )
            };

            // Store content area geometry for scrollbar mouse event handling.
            self.content_origin_row = content_box.style_adjusted_origin_pos.row_index.as_u16();
            self.content_origin_col = content_box.style_adjusted_origin_pos.col_index.as_u16();
            self.content_col_count = content_box.style_adjusted_bounds_size.col_width.as_u16();
            self.content_row_count = content_box.style_adjusted_bounds_size.row_height.as_u16();

            // Push title ops first (display behind content).
            if title_height > 0 {
                global_data.pipeline.push(ZOrder::Normal, title_ops);
            }

            // Render active child component (pushes into global_data.pipeline).
            match active_window {
                Some(Window::FileNamePicker) => {
                    self.picker
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                Some(Window::ThemePicker) => self.theme_picker.render(
                    global_data,
                    inner_bounds,
                    surface_bounds,
                    has_focus,
                )?,
                Some(Window::FilePreview(_)) => {
                    self.preview
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                Some(Window::Terminal(_)) => {
                    self.terminal
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                None => {}
            };

            // Render scrollbar on the rightmost column if there's an active window.
            if let Some(window) = active_window {
                let (scroll, scroll_max, page_size) = match window {
                    Window::Terminal(id) => {
                        let Some(pane) = global_data.state.terminal_panes.get(&id) else {
                            return Ok(());
                        };
                        let Ok(pane) = pane.lock() else {
                            return Ok(());
                        };
                        if pane.ofs_buf.terminal_mode.active_screen_buffer
                            == ActiveScreenBuffer::Alternate
                        {
                            return Ok(());
                        }
                        let scrollback_len = pane.ofs_buf.scrollback_len();
                        let buffer_height = pane.ofs_buf.get_window_size().row_height.as_usize();
                        let scroll =
                            scrollback_len.saturating_sub(pane.scroll_offset.min(scrollback_len));
                        (scroll, scrollback_len + buffer_height, buffer_height)
                    }
                    _ => {
                        let state = &global_data.state;
                        (
                            state.pane_manager.window_scroll(&window),
                            state.pane_manager.window_scroll_max(&window),
                            state.pane_manager.window_page_size(&window),
                        )
                    }
                };
                let mut scrollbar_ops = RenderOpIRVec::new();
                render_scrollbar(
                    &mut scrollbar_ops,
                    &content_box,
                    scroll,
                    scroll_max,
                    page_size,
                    &global_data.state.theme,
                );
                global_data.pipeline.push(ZOrder::Normal, scrollbar_ops);
            }
        });
    }
}
fn render_scrollbar(
    render_ops: &mut RenderOpIRVec,
    content_box: &FlexBox,
    scroll: usize,
    scroll_max: usize,
    page_size: usize,
    theme: &HelixTheme,
) {
    let visible_rows = content_box.style_adjusted_bounds_size.row_height.as_usize();
    if visible_rows == 0 {
        return;
    }

    let origin = content_box.style_adjusted_origin_pos;
    let scroll_col =
        (content_box.style_adjusted_bounds_size.col_width.as_usize()).saturating_sub(1);

    // Track: theme scrollbar bg -> virtual ruler -> default.
    let track_rgb = theme
        .ui_bg("ui.menu.scroll")
        .or_else(|| theme.ui_bg("ui.virtual.ruler"))
        .unwrap_or([30, 30, 50]);

    // Thumb: theme scrollbar fg -> selection -> cursorline -> default.
    let mut thumb_rgb = theme
        .ui_fg("ui.menu.scroll")
        .or_else(|| theme.ui_bg("ui.selection"))
        .or_else(|| theme.ui_bg("ui.cursorline.primary"))
        .or_else(|| theme.ui_bg("ui.cursorline"))
        .unwrap_or([50, 50, 90]);

    // If thumb and track are the same, force contrast using cursor/text accent.
    if thumb_rgb == track_rgb {
        thumb_rgb = theme
            .ui_bg("ui.cursor")
            .or_else(|| theme.ui_fg("ui.text.focus"))
            .or_else(|| theme.ui_fg("ui.text"))
            .unwrap_or([120, 120, 160]);
    }
    // Last resort: if every theme fallback still collides, hardcode contrast.
    if thumb_rgb == track_rgb {
        thumb_rgb = [120, 120, 160];
    }
    let track_bg = tui_color!(track_rgb[0], track_rgb[1], track_rgb[2]);
    let thumb_bg = tui_color!(thumb_rgb[0], thumb_rgb[1], thumb_rgb[2]);

    // Double the vertical resolution using half-block characters.
    let sub_rows = visible_rows * 2;
    let sub_thumb = std::cmp::max(1, (sub_rows * page_size) / scroll_max.max(1));
    let sub_thumb_start = if scroll_max <= page_size {
        0
    } else {
        (scroll * (sub_rows - sub_thumb)) / (scroll_max - page_size)
    };

    for row_offset in 0..visible_rows {
        let sub_top = row_offset * 2;
        let sub_bot = row_offset * 2 + 1;

        let in_top = sub_top >= sub_thumb_start && sub_top < sub_thumb_start + sub_thumb;
        let in_bot = sub_bot >= sub_thumb_start && sub_bot < sub_thumb_start + sub_thumb;

        let (ch, bg) = match (in_top, in_bot) {
            (true, true) => ('█', thumb_bg),
            (true, false) => ('▀', thumb_bg),
            (false, true) => ('▄', thumb_bg),
            (false, false) => (' ', track_bg),
        };

        let style = new_style!(color_fg: {bg} color_bg: {track_bg});
        *render_ops += RenderOpCommon::MoveCursorPositionRelTo(
            origin,
            col(scroll_col as u16) + row(row_offset),
        );
        *render_ops += RenderOpCommon::ApplyColors(Some(style));
        *render_ops += RenderOpIR::PaintTextWithAttributes(ch.to_string().into(), Some(style));
    }
}

pub(crate) fn pane_slot(id: FlexBoxId) -> Option<usize> {
    match id.inner {
        x if x == Id::Pane0 as u8 => Some(0),
        x if x == Id::Pane1 as u8 => Some(1),
        x if x == Id::Pane2 as u8 => Some(2),
        x if x == Id::Pane3 as u8 => Some(3),
        x if x == Id::Pane4 as u8 => Some(4),
        x if x == Id::Pane5 as u8 => Some(5),
        x if x == Id::Pane6 as u8 => Some(6),
        x if x == Id::Pane7 as u8 => Some(7),
        x if x == Id::Pane8 as u8 => Some(8),
        x if x == Id::Pane9 as u8 => Some(9),
        x if x == Id::Pane10 as u8 => Some(10),
        x if x == Id::Pane11 as u8 => Some(11),
        x if x == Id::Pane12 as u8 => Some(12),
        x if x == Id::Pane13 as u8 => Some(13),
        x if x == Id::Pane14 as u8 => Some(14),
        x if x == Id::Pane15 as u8 => Some(15),
        _ => None,
    }
}
