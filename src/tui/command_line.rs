use crate::config::Alias;
use crate::tui::after_agents::{self, AfterAgents};
use crate::tui::*;
use camino::{Utf8Path, Utf8PathBuf};

const PROMPT: &str = ":";
const PLACEHOLDER: &str = "<shell command>";
const ALIAS: &str = "alias";
const TRUST: &str = "trust";
const UNTRUST: &str = "untrust";
const SCAN: &str = "scan";
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
    let file = match command_line.origin {
        Some(Window::FilePreview(key)) => {
            state.files.load().get(key.0).map(|file| file.path.clone())
        }
        _ => None,
    };
    let word = text.split_whitespace().next().unwrap_or_default();
    let argument = text.trim_start().strip_prefix(word).unwrap_or_default();
    let outcome = match word {
        after_agents::NAME => {
            set_after_agents(state, argument.trim(), file.as_deref()).map(|()| None)
        }
        ALIAS => set_alias(state, argument.trim()).map(|()| None),
        SCAN => {
            scan_into_report(state);
            Ok(None)
        }
        TRUST => set_trust(state, true).map(|()| None),
        UNTRUST => set_trust(state, false).map(|()| None),
        _ => match text.strip_prefix('!') {
            Some(shell_cmd) => shell_signal(shell_cmd, file.as_deref(), state).map(Some),
            None => Err(format!(
                "unknown command: {}",
                text.trim().lines().next().unwrap_or_default()
            )),
        },
    };
    match outcome {
        Ok(signal) => {
            state.command_line = None;
            if let Some(signal) = signal {
                send_signal!(
                    global_data.main_thread_channel_sender,
                    TerminalWindowMainThreadSignal::ApplyAppSignal(signal)
                );
            }
        }
        Err(error) => {
            if let Some(command_line) = state.command_line.as_mut() {
                command_line.error = Some(error);
            }
        }
    }
}

/// Scans the repository and shows what it found in a report window. Runs in place: a scan
/// of a large repository takes a fraction of a second, and nothing is written anywhere.
pub(crate) fn scan_into_report(state: &mut AppState) {
    let report = crate::scan::scan(&state.root);
    let danger = report.count(crate::scan::Severity::Danger);
    let title = match danger {
        0 => "Repository scan: nothing hidden found".to_string(),
        1 => "Repository scan: 1 danger finding".to_string(),
        count => format!("Repository scan: {count} danger findings"),
    };
    crate::tui::report_pane::ReportComponent::show(state, title, report.to_markdown());
}

/// Allows or forbids agent turns in this repository, for the session this writes.
fn set_trust(state: &mut AppState, trusted: bool) -> Result<(), String> {
    if state.trusted == trusted {
        return Err(if trusted {
            "this repository is already trusted".to_string()
        } else {
            "this repository is not trusted".to_string()
        });
    }
    state.trusted = trusted;
    state.mark_session_dirty();
    Ok(())
}

fn set_after_agents(
    state: &mut AppState,
    command: &str,
    file: Option<&Utf8Path>,
) -> Result<(), String> {
    if command.is_empty() {
        state.after_agents = None;
        return Ok(());
    }
    let command = resolve_alias(state, command);
    let expanded = expand_placeholders(&command, file, &state.root)?;
    state.after_agents = Some(AfterAgents::Pending(expanded));
    Ok(())
}

/// Sets, replaces or (with no command) removes a project alias.
fn set_alias(state: &mut AppState, argument: &str) -> Result<(), String> {
    let Some((name, command)) = argument
        .split_once(char::is_whitespace)
        .map(|(name, command)| (name, command.trim()))
        .or((!argument.is_empty()).then_some((argument, "")))
    else {
        return Ok(());
    };
    if command.is_empty() {
        if state.project_aliases.iter().any(|alias| alias.name == name) {
            state.project_aliases.retain(|alias| alias.name != name);
        } else if state.config_aliases.iter().any(|alias| alias.name == name) {
            return Err(format!("{name} is defined in config.kdl"));
        } else {
            return Err(format!("no alias {name}"));
        }
    } else if let Some(alias) = state
        .project_aliases
        .iter_mut()
        .find(|alias| alias.name == name)
    {
        alias.command = command.to_string();
    } else {
        state.project_aliases.push(Alias {
            name: name.to_string(),
            command: command.to_string(),
        });
    }
    state.mark_session_dirty();
    Ok(())
}

/// `text` with its first word replaced by the alias it names, the project's before the
/// config's; the rest is appended as typed. A miss returns `text` unchanged.
pub fn resolve_alias(state: &AppState, text: &str) -> String {
    let trimmed = text.trim_start();
    let (word, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim_start()),
        None => (trimmed, ""),
    };
    let alias = state
        .project_aliases
        .iter()
        .chain(&state.config_aliases)
        .find(|alias| alias.name == word);
    match alias {
        Some(alias) if rest.is_empty() => alias.command.clone(),
        Some(alias) => format!("{} {rest}", alias.command),
        None => text.to_string(),
    }
}

/// ` · aliases: a, b` for the placeholder of the commands that take a shell command.
fn aliases_suffix(state: &AppState) -> String {
    let mut names: Vec<&str> = Vec::new();
    for alias in state.project_aliases.iter().chain(&state.config_aliases) {
        if !names.contains(&alias.name.as_str()) {
            names.push(&alias.name);
        }
    }
    if names.is_empty() {
        String::new()
    } else {
        format!(" · aliases: {}", names.join(", "))
    }
}

/// The placeholder for a command typed alone (`!`, `after-agents`, `alias [name]`).
fn command_hint(typed: &str, state: &AppState) -> Option<String> {
    let mut words = typed.split_whitespace();
    let hint = match (words.next()?, words.next(), words.next()) {
        ("!", None, _) if typed == "!" => {
            format!("{PLACEHOLDER}{}", aliases_suffix(state))
        }
        (SCAN, None, _) => "read the repository for instructions aimed at an agent".to_string(),
        (TRUST, None, _) => "allow agent turns in this repository".to_string(),
        (UNTRUST, None, _) => "forbid agent turns in this repository".to_string(),
        (after_agents::NAME, None, _) => format!(
            "{}{}",
            after_agents::hint(state.after_agents.as_ref()),
            aliases_suffix(state)
        ),
        (ALIAS, None, _) => {
            let entries: Vec<String> = state
                .project_aliases
                .iter()
                .map(|alias| format!("{} = {}", alias.name, alias.command))
                .chain(
                    state
                        .config_aliases
                        .iter()
                        .filter(|alias| {
                            !state
                                .project_aliases
                                .iter()
                                .any(|own| own.name == alias.name)
                        })
                        .map(|alias| format!("{} = {} (config)", alias.name, alias.command)),
                )
                .collect();
            if entries.is_empty() {
                "no aliases · alias <name> <shell command>".to_string()
            } else {
                entries.join(" · ")
            }
        }
        (ALIAS, Some(name), None) => {
            match state
                .project_aliases
                .iter()
                .find(|alias| alias.name == name)
            {
                Some(alias) => format!("Enter removes {name} (was: {})", alias.command),
                None => PLACEHOLDER.to_string(),
            }
        }
        _ => return None,
    };
    let gap = if typed.ends_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    Some(format!("{gap}{hint}"))
}

/// Paints over the bottom rows, growing upward: the panes are not resized while typing.
pub fn render_command_line(pipeline: &mut RenderPipeline, size: Size, state: &mut AppState) {
    let hint = state
        .command_line
        .as_ref()
        .and_then(|command_line| command_hint(command_line.input.text(), state));
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
    let placeholder = match &command_line.error {
        Some(error) => {
            let error_fg = TuiColor::from(theme.ui_fg("error").unwrap_or(DEFAULT_ERROR_FG));
            Some((format!("  {error}"), error_fg))
        }
        None => hint.map(|hint| {
            let weak = theme.color_for_scope("comment").unwrap_or(fg_rgb);
            (hint, TuiColor::from(weak))
        }),
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
    state: &AppState,
) -> Result<AppSignal, String> {
    let cmd = if shell_cmd.is_empty() {
        None
    } else {
        Some(expand_placeholders(
            &resolve_alias(state, shell_cmd),
            file,
            &state.root,
        )?)
    };
    Ok(AppSignal::OpenTerminal {
        cmd,
        cwd: state.root.to_path_buf(),
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

/// Expands `%%`, `%`, `%c`/`%crate`, `%r`/`%repo` and `%project` placeholders in `cmd`.
///
/// - `%%`           → a literal `%`
/// - `%project`     → shell-quoted name of the directory of `root`; needs no file
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
        if s.starts_with("project") && is_word_end(&s[7..]) {
            result.push_str(&sh_quote(root.file_name().unwrap_or_default()));
            s = &s[7..];
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
    fn project_is_the_root_directory_name_and_needs_no_file() {
        let root = Utf8Path::new("/home/me/my repo");
        let file = Utf8Path::new("/home/me/my repo/src/main.rs");
        assert_eq!(
            expand_placeholders("notify-send %project", None, root),
            Ok("notify-send 'my repo'".to_string())
        );
        assert_eq!(
            expand_placeholders("echo %project %", Some(file), root),
            Ok("echo 'my repo' 'src/main.rs'".to_string())
        );
    }

    fn alias(name: &str, command: &str) -> Alias {
        Alias {
            name: name.to_string(),
            command: command.to_string(),
        }
    }

    fn state_with_aliases() -> AppState {
        AppState {
            config_aliases: vec![
                alias("poweroff", "systemctl poweroff"),
                alias("notify", "notify-send %project"),
            ],
            project_aliases: vec![alias("notify", "echo project")],
            ..AppState::default()
        }
    }

    #[test]
    fn alias_takes_the_rest_as_typed() {
        let state = state_with_aliases();
        assert_eq!(resolve_alias(&state, "poweroff"), "systemctl poweroff");
        assert_eq!(
            resolve_alias(&state, "notify  \"build done\" now "),
            "echo project \"build done\" now "
        );
        assert_eq!(resolve_alias(&state, "notifying x"), "notifying x");
        assert_eq!(resolve_alias(&state, " git push "), " git push ");
    }

    #[test]
    fn only_the_first_word_is_an_alias() {
        let state = state_with_aliases();
        assert_eq!(resolve_alias(&state, "ls && poweroff"), "ls && poweroff");
    }

    #[test]
    fn alias_sets_replaces_and_removes_a_project_alias() {
        let mut state = state_with_aliases();
        set_alias(&mut state, "ci cargo test").unwrap();
        assert_eq!(state.project_aliases[1], alias("ci", "cargo test"));
        set_alias(&mut state, "ci cargo nextest run").unwrap();
        assert_eq!(state.project_aliases.len(), 2);
        assert_eq!(state.project_aliases[1].command, "cargo nextest run");
        set_alias(&mut state, "ci").unwrap();
        assert_eq!(state.project_aliases, [alias("notify", "echo project")]);
        assert!(state.session_dirty_at.is_some());
    }

    #[test]
    fn alias_removal_errors() {
        let mut state = state_with_aliases();
        assert_eq!(
            set_alias(&mut state, "poweroff"),
            Err("poweroff is defined in config.kdl".to_string())
        );
        assert_eq!(
            set_alias(&mut state, "nope"),
            Err("no alias nope".to_string())
        );
        assert_eq!(set_alias(&mut state, ""), Ok(()));
        assert_eq!(state.project_aliases.len(), 1);
    }

    #[test]
    fn hints_list_and_name_aliases() {
        let state = state_with_aliases();
        assert_eq!(
            command_hint("!", &state).unwrap(),
            " <shell command> · aliases: notify, poweroff"
        );
        assert_eq!(
            command_hint("alias", &state).unwrap(),
            " notify = echo project · poweroff = systemctl poweroff (config)"
        );
        assert_eq!(
            command_hint("alias notify", &state).unwrap(),
            " Enter removes notify (was: echo project)"
        );
        assert_eq!(command_hint("ls", &state), None);
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
