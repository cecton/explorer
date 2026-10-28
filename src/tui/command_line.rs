use crate::tui::*;
use camino::{Utf8Path, Utf8PathBuf};

const PROMPT: &str = ":";
const PLACEHOLDER: &str = "<shell command>";
const MAX_ROWS: usize = 10;

/// The global command line opened with `<leader>:`, painted over the status bar.
#[derive(Clone)]
pub struct CommandLine {
    pub input: InputLine,
    /// The window focused when the command line opened: focus follows the mouse meanwhile.
    pub origin: Option<Window>,
    pub error: Option<String>,
}

impl CommandLine {
    pub fn new(origin: Option<Window>) -> Self {
        Self {
            input: InputLine::new(),
            origin,
            error: None,
        }
    }
}

/// Runs the typed command: the command line closes, or stays open with the error.
pub fn run_command_line(global_data: &mut GlobalData<AppState, AppSignal>) {
    let state = &mut global_data.state;
    let Some(command_line) = state.command_line.as_ref() else {
        return;
    };
    let text = command_line.input.text().to_string();
    if text.trim().is_empty() {
        state.command_line = None;
        return;
    }
    let outcome = match text.strip_prefix('!') {
        Some(shell_cmd) => {
            let file = match command_line.origin {
                Some(Window::FilePreview(key)) => {
                    state.files.load().get(key.0).map(|file| file.path.clone())
                }
                _ => None,
            };
            shell_signal(shell_cmd, file.as_deref(), &state.root)
        }
        None => Err(format!(
            "unknown command: {}",
            text.trim().lines().next().unwrap_or_default()
        )),
    };
    match outcome {
        Ok(signal) => {
            state.command_line = None;
            send_signal!(
                global_data.main_thread_channel_sender,
                TerminalWindowMainThreadSignal::ApplyAppSignal(signal)
            );
        }
        Err(error) => {
            if let Some(command_line) = state.command_line.as_mut() {
                command_line.error = Some(error);
            }
        }
    }
}

/// Paints over the bottom rows, growing upward: the panes are not resized while typing.
pub fn render_command_line(pipeline: &mut RenderPipeline, size: Size, state: &mut AppState) {
    let AppState {
        command_line: Some(command_line),
        theme,
        ..
    } = state
    else {
        return;
    };
    let (bg_rgb, fg_rgb) = title_bar_colors(false, theme);
    let color_bg = TuiColor::from(bg_rgb);
    let color_fg = TuiColor::from(fg_rgb);
    let color_selection = TuiColor::from(title_selection_bg(false, theme));
    let after_bang = command_line.input.text() == "!";
    let placeholder = match &command_line.error {
        Some(error) => {
            let error_fg = TuiColor::from(theme.ui_fg("error").unwrap_or(DEFAULT_ERROR_FG));
            Some((format!("  {error}"), error_fg))
        }
        None if after_bang => {
            let weak = theme.color_for_scope("comment").unwrap_or(fg_rgb);
            Some((PLACEHOLDER.to_string(), TuiColor::from(weak)))
        }
        None => None,
    };

    let width = size.col_width.as_u16();
    let screen_rows = size.row_height.as_usize();
    let max_rows = MAX_ROWS.min(screen_rows / 3).max(1);
    let rows = command_line.input.height(
        placeholder.as_ref().map(|(text, _)| text.as_str()),
        PROMPT,
        width,
        max_rows,
    );
    let top = screen_rows.saturating_sub(rows);

    let fill_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});
    let mut ops = RenderOpIRVec::new();
    for row_idx in top..screen_rows {
        ops += RenderOpCommon::MoveCursorPositionAbs(col(0) + row(row_idx as u16));
        ops += RenderOpCommon::ApplyColors(Some(fill_style));
        ops += RenderOpIR::PaintTextWithAttributes(
            " ".repeat(width as usize).as_str().into(),
            Some(fill_style),
        );
    }
    command_line.input.render(
        &mut ops,
        placeholder
            .as_ref()
            .map(|(text, color)| (text.as_str(), *color)),
        PROMPT,
        col(0) + row(top as u16),
        width,
        rows,
        true,
        (color_bg, color_fg, color_selection),
    );
    pipeline.push(ZOrder::Normal, ops);
}

/// The terminal a `!` command opens: a plain shell when the command is empty.
pub fn shell_signal(
    shell_cmd: &str,
    file: Option<&Utf8Path>,
    root: &Utf8Path,
) -> Result<AppSignal, String> {
    let cmd = if shell_cmd.is_empty() {
        None
    } else {
        Some(expand_placeholders(shell_cmd, file, root)?)
    };
    Ok(AppSignal::OpenTerminal {
        cmd,
        cwd: root.to_path_buf(),
    })
}

/// Wraps a string in single quotes for safe embedding in a `/bin/sh -c` command string.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Returns true if `s` does not start with an alphanumeric character or `_`,
/// i.e. the previous token ended at a word boundary.
fn is_word_end(s: &str) -> bool {
    !s.starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

/// Finds the nearest ancestor directory of `file_path` that contains a `Cargo.toml`.
fn find_crate_root(file_path: &Utf8Path) -> Option<Utf8PathBuf> {
    let mut dir = file_path.parent()?;
    loop {
        if dir.join("Cargo.toml").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Finds the nearest ancestor directory of `file_path` that contains a `.git` directory.
fn find_repo_root(file_path: &Utf8Path) -> Option<Utf8PathBuf> {
    let mut dir = file_path.parent()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Expands `%%`, `%`, `%c`/`%crate`, and `%r`/`%repo` placeholders in `cmd`.
///
/// - `%%`           → a literal `%`
/// - `%`            → shell-quoted relative path of `file` from `root`
/// - `%d` / `%dir`   → shell-quoted directory of `file` (relative to `root`)
/// - `%c` / `%crate` → shell-quoted path to the nearest `Cargo.toml` ancestor directory
/// - `%r` / `%repo`  → shell-quoted path to the nearest `.git` ancestor directory
///
/// Returns an error string if a placeholder is used without a file or its root cannot
/// be found.
fn expand_placeholders(
    cmd: &str,
    file: Option<&Utf8Path>,
    root: &Utf8Path,
) -> Result<String, String> {
    let mut result = String::with_capacity(cmd.len());
    let mut s = cmd;
    while !s.is_empty() {
        let Some(pct) = s.find('%') else {
            result.push_str(s);
            break;
        };
        result.push_str(&s[..pct]);
        s = &s[pct + 1..];

        if let Some(rest) = s.strip_prefix('%') {
            result.push('%');
            s = rest;
            continue;
        }
        let file_path = file.ok_or_else(|| "% needs a file preview in focus".to_string())?;
        if s.starts_with("crate") && is_word_end(&s[5..]) {
            let root = find_crate_root(file_path)
                .ok_or_else(|| "no Cargo.toml found (needed by %crate)".to_string())?;
            result.push_str(&sh_quote(root.as_str()));
            s = &s[5..];
        } else if s.starts_with("repo") && is_word_end(&s[4..]) {
            let root = find_repo_root(file_path)
                .ok_or_else(|| "no .git found (needed by %repo)".to_string())?;
            result.push_str(&sh_quote(root.as_str()));
            s = &s[4..];
        } else if s.starts_with('c') && is_word_end(&s[1..]) {
            let root = find_crate_root(file_path)
                .ok_or_else(|| "no Cargo.toml found (needed by %c)".to_string())?;
            result.push_str(&sh_quote(root.as_str()));
            s = &s[1..];
        } else if s.starts_with("dir") && is_word_end(&s[3..]) {
            let parent = file_path.parent().unwrap_or(file_path);
            let rel = parent.strip_prefix(root).unwrap_or(parent);
            result.push_str(&sh_quote(rel.as_str()));
            s = &s[3..];
        } else if s.starts_with('r') && is_word_end(&s[1..]) {
            let root = find_repo_root(file_path)
                .ok_or_else(|| "no .git found (needed by %r)".to_string())?;
            result.push_str(&sh_quote(root.as_str()));
            s = &s[1..];
        } else if s.starts_with('d') && is_word_end(&s[1..]) {
            let parent = file_path.parent().unwrap_or(file_path);
            let rel = parent.strip_prefix(root).unwrap_or(parent);
            result.push_str(&sh_quote(rel.as_str()));
            s = &s[1..];
        } else {
            let rel = file_path.strip_prefix(root).unwrap_or(file_path);
            result.push_str(&sh_quote(rel.as_str()));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_percent_is_a_literal_percent() {
        let root = Utf8Path::new("/repo");
        let file = Utf8Path::new("/repo/src/main.rs");
        assert_eq!(
            expand_placeholders("date +%%Y %", Some(file), root),
            Ok("date +%Y 'src/main.rs'".to_string())
        );
        assert_eq!(
            expand_placeholders("printf '%%s\\n' x", None, root),
            Ok("printf '%s\\n' x".to_string())
        );
    }

    #[test]
    fn placeholder_without_a_file_is_refused() {
        let root = Utf8Path::new("/repo");
        assert_eq!(
            expand_placeholders("cat %", None, root),
            Err("% needs a file preview in focus".to_string())
        );
        assert_eq!(expand_placeholders("ls", None, root), Ok("ls".to_string()));
    }
}
