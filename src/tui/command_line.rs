#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

const PROMPT: &str = ":";
const PLACEHOLDER: &str = "<shell command>";
/// The name of the command that marks lines of the file in focus: the file preview's `:` opens
/// the line with it typed.
pub(crate) const HIGHLIGHT: &str = "highlight";
const MAX_ROWS: usize = 10;
const MAX_LIST_ROWS: usize = 5;

/// A command of the line. Everything the line knows about it lives here: the palette lists
/// it and `run` is what the line does with what follows its name.
pub(crate) struct Command {
    pub(crate) name: &'static str,
    /// What the command expects after its name, as the palette spells it out.
    argument: &'static str,
    /// One line, used by the palette and by the placeholder of the command typed alone.
    description: &'static str,
    run: fn(&mut CommandRun<'_>) -> Result<(), String>,
}

/// What a command is handed: the state it acts on, its argument, and the ways out.
struct CommandRun<'a> {
    state: &'a mut crate::tui::state::AppState,
    /// The text after the command's name, trimmed.
    argument: &'a str,
    /// The file in focus, for the `%` placeholders.
    file: Option<&'a camino::Utf8Path>,
    /// The window focused when the line opened.
    origin: Option<crate::tui::pane_manager::Window>,
    /// The main thread's channel, which an MCP server reports its status through.
    main_tx:
        tokio::sync::mpsc::Sender<TerminalWindowMainThreadSignal<crate::tui::state::AppSignal>>,
    /// The terminal a command asks for, applied once the line has closed.
    signal: Option<crate::tui::state::AppSignal>,
}

/// Every command the line runs, in the order the palette lists it.
const COMMANDS: [Command; 10] = [
    Command {
        name: crate::tui::after_agents::NAME,
        argument: "[shell command]",
        description: "run a shell command once the agents are done",
        run: run_after_agents,
    },
    Command {
        name: "alias",
        argument: "[name] [shell command]",
        description: "set, replace or remove a project alias",
        run: run_alias,
    },
    Command {
        name: "all-features",
        argument: "",
        description: "analyze every Cargo feature with rust-analyzer",
        run: run_all_features,
    },
    Command {
        name: "default-features",
        argument: "",
        description: "analyze the default Cargo features with rust-analyzer",
        run: run_default_features,
    },
    Command {
        name: "features",
        argument: "[no-default] [feature...]",
        description: "choose the Cargo features rust-analyzer analyzes",
        run: run_features,
    },
    Command {
        name: HIGHLIGHT,
        argument: "[lines]",
        description: "mark lines of the file in focus: 12, 3-7, 1-3,8; bare clears",
        run: run_highlight,
    },
    Command {
        name: "mcp",
        argument: "[add <name> <program> | remove <name>]",
        description: "MCP servers of this repository",
        run: run_mcp,
    },
    Command {
        name: "scan",
        argument: "",
        description: "read the repository for instructions aimed at an agent",
        run: run_scan,
    },
    Command {
        name: "trust",
        argument: "",
        description: "allow agent turns in this repository",
        run: run_trust,
    },
    Command {
        name: "untrust",
        argument: "",
        description: "forbid agent turns in this repository",
        run: run_untrust,
    },
];

/// An alias the palette offers: a name typed on the line, which opens its command in a
/// terminal. Aliases belong to the session, so they are only ever what the palette lists.
pub(crate) struct AliasRow {
    name: String,
    command: String,
    from_config: bool,
}

/// One row of the palette, whatever it came from.
pub(crate) enum Entry {
    Command(&'static Command),
    Alias(AliasRow),
}

impl Entry {
    fn name(&self) -> &str {
        match self {
            Self::Command(command) => command.name,
            Self::Alias(alias) => &alias.name,
        }
    }

    /// What the row says after the name.
    fn detail(&self) -> String {
        match self {
            Self::Command(command) if command.argument.is_empty() => {
                command.description.to_string()
            }
            Self::Command(command) => format!("{}   {}", command.argument, command.description),
            Self::Alias(alias) => {
                let origin = if alias.from_config { " (config)" } else { "" };
                format!("= {}{origin}", alias.command)
            }
        }
    }

    /// What goes into the input: the name, then a space for what takes an argument.
    fn completion(&self) -> String {
        match self {
            Self::Command(command) if command.argument.is_empty() => command.name.to_string(),
            Self::Command(command) => format!("{} ", command.name),
            Self::Alias(alias) => format!("{} ", alias.name),
        }
    }
}

/// The global command line opened with `<leader>:`, painted over the status bar.
#[derive(Clone)]
pub struct CommandLine {
    pub input: crate::tui::input_line::InputLine,
    /// The window focused when the command line opened: focus follows the mouse meanwhile.
    pub origin: Option<crate::tui::pane_manager::Window>,
    pub error: Option<String>,
    /// The palette's highlight, an index into its entries. Reset whenever the text changes.
    pub selected: usize,
}

impl CommandLine {
    pub fn new(origin: Option<crate::tui::pane_manager::Window>) -> Self {
        Self::prefilled(origin, "")
    }

    /// A line that opens with `text` already typed.
    pub fn prefilled(
        origin: Option<crate::tui::pane_manager::Window>,
        text: impl Into<String>,
    ) -> Self {
        use crate::tui::input_line::InputLine;
        Self {
            input: InputLine::with_text(text),
            origin,
            error: None,
            selected: 0,
        }
    }
}

/// The aliases of the repository, project ones first, the config's shadowed by them.
fn alias_rows(state: &crate::tui::state::AppState) -> Vec<AliasRow> {
    state
        .project_aliases
        .iter()
        .map(|alias| AliasRow {
            name: alias.name.clone(),
            command: alias.command.clone(),
            from_config: false,
        })
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
                .map(|alias| AliasRow {
                    name: alias.name.clone(),
                    command: alias.command.clone(),
                    from_config: true,
                }),
        )
        .collect()
}

/// Whether `word` names a project or config alias.
fn is_alias(state: &crate::tui::state::AppState, word: &str) -> bool {
    state
        .project_aliases
        .iter()
        .chain(&state.config_aliases)
        .any(|alias| alias.name == word)
}

/// The palette's entries for the first word `typed`: the commands, then the aliases, whose
/// names are prefix-matched too since typing one is how it runs. Only while that word is
/// being typed — once it is followed by a space the line takes an argument, and once it
/// names something exactly the line's own hint says more than a one-line description.
pub(crate) fn palette_entries(typed: &str, state: &crate::tui::state::AppState) -> Vec<Entry> {
    if typed.contains(char::is_whitespace) || typed.starts_with('!') {
        return Vec::new();
    }
    if COMMANDS.iter().any(|command| command.name == typed) || is_alias(state, typed) {
        return Vec::new();
    }
    let mut entries: Vec<Entry> = COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(typed))
        .map(Entry::Command)
        .collect();
    entries.extend(
        alias_rows(state)
            .into_iter()
            .filter(|alias| alias.name.starts_with(typed))
            .map(Entry::Alias),
    );
    entries
}

/// The palette as it stands: its entries, and which of them is highlighted.
struct Palette {
    entries: Vec<Entry>,
    selected: usize,
}

impl Palette {
    /// `None` when nothing matches, so the palette has nothing to show.
    fn new(typed: &str, selected: usize, state: &crate::tui::state::AppState) -> Option<Self> {
        let entries = palette_entries(typed, state);
        let last = entries.len().checked_sub(1)?;
        Some(Self {
            entries,
            selected: selected.min(last),
        })
    }
}

impl CommandLine {
    /// Take the entry at `selected` as the typed text, so a second Enter runs it. A name
    /// that takes an argument is only written out, with a space after it: the input never
    /// holds a command the user has not seen.
    pub fn accept(&mut self, entries: &[Entry]) {
        let Some(entry) = entries.get(self.selected) else {
            return;
        };
        self.input.set_text(entry.completion());
        self.selected = 0;
    }

    /// Moves the palette's highlight, wrapping, and returns whether it moved.
    pub fn move_selection(&mut self, down: bool, entries: usize) -> bool {
        if entries == 0 {
            return false;
        }
        self.selected = if down {
            (self.selected + 1) % entries
        } else {
            (self.selected + entries - 1) % entries
        };
        true
    }
}

/// Runs the typed command: the command line closes, or stays open with the error.
pub fn run_command_line(
    global_data: &mut GlobalData<crate::tui::state::AppState, crate::tui::state::AppSignal>,
) {
    use crate::tui::pane_manager::Window;
    let main_tx = global_data.main_thread_channel_sender.clone();
    let state = &mut global_data.state;
    let Some(command_line) = state.command_line.as_ref() else {
        return;
    };
    let text = command_line.input.text().to_string();
    if text.trim().is_empty() {
        state.command_line = None;
        return;
    }
    let origin = command_line.origin;
    let file = match origin {
        Some(Window::FilePreview(key)) => {
            state.files.load().get(key.0).map(|file| file.path.clone())
        }
        _ => None,
    };
    let word = text.split_whitespace().next().unwrap_or_default();
    let argument = text
        .trim_start()
        .strip_prefix(word)
        .unwrap_or_default()
        .trim();
    let mut run = CommandRun {
        state,
        argument,
        file: file.as_deref(),
        origin,
        main_tx,
        signal: None,
    };
    let outcome = match COMMANDS.iter().find(|command| command.name == word) {
        Some(command) => (command.run)(&mut run),
        None if word.starts_with('!') => shell_signal(
            text.trim().trim_start_matches('!'),
            file.as_deref(),
            run.state,
        )
        .map(|signal| run.signal = Some(signal)),
        // A name the palette lists as an alias opens its command in a terminal: typing the
        // name is how it runs, so the palette must not offer what the line would refuse.
        None if is_alias(run.state, word) => {
            shell_signal(&text, file.as_deref(), run.state).map(|signal| run.signal = Some(signal))
        }
        None => Err(format!(
            "unknown command: {}",
            text.trim().lines().next().unwrap_or_default()
        )),
    };
    match outcome {
        Ok(()) => {
            let signal = run.signal.take();
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

fn run_after_agents(run: &mut CommandRun<'_>) -> Result<(), String> {
    use crate::tui::after_agents::AfterAgents;
    if run.argument.is_empty() {
        run.state.after_agents = None;
        return Ok(());
    }
    let command = resolve_alias(run.state, run.argument);
    let expanded = expand_placeholders(&command, run.file, &run.state.root)?;
    run.state.after_agents = Some(AfterAgents::Pending(expanded));
    Ok(())
}

fn run_alias(run: &mut CommandRun<'_>) -> Result<(), String> {
    set_alias(run.state, run.argument)
}

/// Marks lines of the file preview the line opened from, and scrolls to them.
fn run_highlight(run: &mut CommandRun<'_>) -> Result<(), String> {
    use crate::tui::pane_manager::Window;
    let Some(window @ Window::FilePreview(key)) = run.origin else {
        return Err("highlight needs a file preview in focus".to_string());
    };
    if run.argument.is_empty() {
        if run.state.highlight_ranges.remove(&key).is_some() {
            run.state.mark_session_dirty();
        }
        return Ok(());
    }
    let max = run.state.pane_manager.window_scroll_max(&window);
    let ranges = parse_line_ranges(run.argument, max)
        .ok_or_else(|| "highlight: expected lines like 12, 3-7 or 1-3,8".to_string())?;
    let page_size = run.state.pane_manager.window_page_size(&window);
    let target = crate::tui::preview::compute_scroll_target(&ranges, page_size, max);
    run.state.highlight_ranges.insert(key, ranges);
    run.state.pane_manager.set_window_scroll(&window, target);
    run.state.pane_manager.clamp_scroll(&window);
    run.state.mark_session_dirty();
    Ok(())
}

/// The ranges `text` names, comma-separated lines (`12`) or ranges (`3-7`) of a file of
/// `max` lines, clamped to it; what is not a line number is skipped. `None` when nothing
/// is left.
fn parse_line_ranges(text: &str, max: usize) -> Option<Vec<(usize, usize)>> {
    if max == 0 {
        return None;
    }
    let mut ranges = Vec::new();
    for spec in text.split(',').map(str::trim) {
        if let Some((start, end)) = spec.split_once('-') {
            let (Ok(start), Ok(end)) = (start.trim().parse::<usize>(), end.trim().parse::<usize>())
            else {
                continue;
            };
            if start > 0 && end > 0 {
                ranges.push((start.min(end).min(max), start.max(end).min(max)));
            }
        } else if let Ok(line) = spec.parse::<usize>()
            && line > 0
            && line <= max
        {
            ranges.push((line, line));
        }
    }
    (!ranges.is_empty()).then_some(ranges)
}

fn run_all_features(run: &mut CommandRun<'_>) -> Result<(), String> {
    use crate::lsp::CargoFeatureSelection;
    select_features(run, CargoFeatureSelection::All)
}

fn run_default_features(run: &mut CommandRun<'_>) -> Result<(), String> {
    use crate::lsp::CargoFeatureSelection;
    select_features(run, CargoFeatureSelection::Default)
}

fn run_features(run: &mut CommandRun<'_>) -> Result<(), String> {
    use crate::lsp::CargoFeatureSelection;
    let mut tokens = run.argument.split_whitespace().peekable();
    let no_default_features = tokens.next_if_eq(&"no-default").is_some();
    let features: Vec<String> = tokens.map(str::to_string).collect();
    if features.is_empty() && !no_default_features {
        return Err("features: expected at least one feature (or `no-default`)".to_string());
    }
    select_features(
        run,
        CargoFeatureSelection::Explicit {
            no_default_features,
            features,
        },
    )
}

/// Asks for rust-analyzer to restart with `selection`, once the line has closed.
fn select_features(
    run: &mut CommandRun<'_>,
    selection: crate::lsp::CargoFeatureSelection,
) -> Result<(), String> {
    use crate::tui::state::AppSignal;
    if crate::lsp::disabled() {
        return Err("rust-analyzer is disabled (EXPLORER_NO_LSP)".to_string());
    }
    run.signal = Some(AppSignal::SetCargoFeatures(selection));
    Ok(())
}

/// Failures are the line's error; a started server is reported in the tree's notice.
fn run_mcp(run: &mut CommandRun<'_>) -> Result<(), String> {
    use crate::agent::mcp::McpServerDecl;
    let words: Vec<&str> = run.argument.split_whitespace().collect();
    match words.as_slice() {
        ["add", name, program] => {
            check_server_name(name, &run.state.mcp_servers)?;
            let decl = McpServerDecl {
                name: name.to_string(),
                program: program.to_string(),
            };
            let notify = crate::tui::app::mcp_notify(run.main_tx.clone());
            run.state.mcp.start(&decl, notify);
            run.state.mcp_servers.push(decl);
            run.state
                .tree_view
                .announced_starts
                .insert(name.to_string());
            run.state.tree_view.notice = Some(format!("{name}: starting"));
            run.state.mark_session_dirty();
            Ok(())
        }
        ["add", ..] => Err("usage: mcp add <name> <program>".to_string()),
        ["remove", name] => {
            if !run
                .state
                .mcp_servers
                .iter()
                .any(|server| server.name == *name)
            {
                return Err(format!("{name}: no such server"));
            }
            run.state.mcp_servers.retain(|server| server.name != *name);
            run.state.mcp.remove(name);
            run.state.tree_view.announced_starts.remove(*name);
            run.state.tree_view.notice = Some(format!("{name}: removed"));
            run.state.mark_session_dirty();
            Ok(())
        }
        ["remove", ..] => Err("usage: mcp remove <name>".to_string()),
        _ => Err("usage: mcp add <name> <program> | mcp remove <name>".to_string()),
    }
}

/// Why `name` cannot name a new MCP server: its tools are `mcp__<name>__<tool>`.
fn check_server_name(
    name: &str,
    servers: &[crate::agent::mcp::McpServerDecl],
) -> Result<(), String> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    if !name.chars().all(allowed) || name.contains("__") {
        return Err(format!(
            "{name}: a server name holds only letters, digits, `_` and `-`, and no `__`"
        ));
    }
    if servers.iter().any(|server| server.name == name) {
        return Err(format!("{name}: a server has this name already"));
    }
    Ok(())
}

fn run_scan(run: &mut CommandRun<'_>) -> Result<(), String> {
    scan_into_report(run.state);
    Ok(())
}

fn run_trust(run: &mut CommandRun<'_>) -> Result<(), String> {
    set_trust(run.state, true)
}

fn run_untrust(run: &mut CommandRun<'_>) -> Result<(), String> {
    set_trust(run.state, false)
}

/// Scans the repository and shows what it found in a report window. Runs in place: a scan
/// of a large repository takes a fraction of a second, and nothing is written anywhere.
pub(crate) fn scan_into_report(state: &mut crate::tui::state::AppState) {
    use crate::scan::Severity;
    use crate::tui::report_pane::ReportComponent;
    let report = crate::scan::scan(&state.root);
    let danger = report.count(Severity::Danger);
    let title = match danger {
        0 => "Repository scan: nothing hidden found".to_string(),
        1 => "Repository scan: 1 danger finding".to_string(),
        count => format!("Repository scan: {count} danger findings"),
    };
    ReportComponent::show(state, title, report.to_markdown());
}

/// Allows or forbids agent turns in this repository, for the session this writes.
fn set_trust(state: &mut crate::tui::state::AppState, trusted: bool) -> Result<(), String> {
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

/// Sets, replaces or (with no command) removes a project alias.
fn set_alias(state: &mut crate::tui::state::AppState, argument: &str) -> Result<(), String> {
    use crate::config::Alias;
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
pub fn resolve_alias(state: &crate::tui::state::AppState, text: &str) -> String {
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
fn aliases_suffix(state: &crate::tui::state::AppState) -> String {
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

/// The placeholder for what is typed: what a command expects, and the aliases in force.
fn command_hint(typed: &str, state: &crate::tui::state::AppState) -> Option<String> {
    use crate::tui::pane_manager::Window;
    let mut words = typed.split_whitespace();
    let hint = match (words.next()?, words.next(), words.next()) {
        ("!", None, _) if typed == "!" => {
            format!("{PLACEHOLDER}{}", aliases_suffix(state))
        }
        (crate::tui::after_agents::NAME, None, _) => format!(
            "{}{}",
            crate::tui::after_agents::hint(state.after_agents.as_ref()),
            aliases_suffix(state)
        ),
        ("alias", None, _) => {
            let entries: Vec<String> = alias_rows(state)
                .iter()
                .map(|alias| {
                    let origin = if alias.from_config { " (config)" } else { "" };
                    format!("{} = {}{origin}", alias.name, alias.command)
                })
                .collect();
            if entries.is_empty() {
                "no aliases · alias <name> <shell command>".to_string()
            } else {
                entries.join(" · ")
            }
        }
        ("alias", Some(name), None) => {
            match state
                .project_aliases
                .iter()
                .find(|alias| alias.name == name)
            {
                Some(alias) => format!("Enter removes {name} (was: {})", alias.command),
                None => PLACEHOLDER.to_string(),
            }
        }
        (HIGHLIGHT, None, _) => {
            let current = match state.command_line.as_ref().and_then(|line| line.origin) {
                Some(Window::FilePreview(key)) => state.highlight_ranges.get(&key),
                _ => None,
            };
            match current {
                Some(ranges) => format!(
                    "current: {}",
                    crate::tui::preview::format_highlight_ranges(ranges)
                ),
                None => "[lines] 12, 3-7, 1-3,8".to_string(),
            }
        }
        ("mcp", None, _) => {
            let installed: Vec<String> = state
                .mcp
                .statuses()
                .iter()
                .map(|(name, status)| format!("{name} ({})", status.label()))
                .collect();
            if installed.is_empty() {
                "Installed: none".to_string()
            } else {
                format!("Installed: {}", installed.join(", "))
            }
        }
        ("mcp", Some("add"), None) => "[name] [program]".to_string(),
        ("mcp", Some("remove"), None) => {
            let statuses = state.mcp.statuses();
            if statuses.is_empty() {
                "(no server installed)".to_string()
            } else {
                let names: Vec<String> = statuses.into_iter().map(|(name, _)| name).collect();
                format!("[{}]", names.join("|"))
            }
        }
        (name, None, _) => match COMMANDS.iter().find(|command| command.name == name) {
            Some(command) => command.description.to_string(),
            None => alias_rows(state)
                .into_iter()
                .find(|alias| alias.name == name)
                .map(|alias| {
                    let origin = if alias.from_config { " (config)" } else { "" };
                    format!("= {}{origin}", alias.command)
                })?,
        },
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
pub fn render_command_line(
    pipeline: &mut RenderPipeline,
    size: VPSize,
    state: &mut crate::tui::state::AppState,
) {
    use crate::tui::state::AppState;
    let palette = state.command_line.as_ref().and_then(|command_line| {
        Palette::new(command_line.input.text(), command_line.selected, state)
    });
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
    let (bg_rgb, fg_rgb) = crate::tui::title_row::title_bar_colors(false, theme);
    let color_bg = TuiColor::from(bg_rgb);
    let color_fg = TuiColor::from(fg_rgb);
    let color_selection = TuiColor::from(crate::tui::title_row::title_selection_bg(false, theme));
    let placeholder = match &command_line.error {
        Some(error) => {
            let error_fg = TuiColor::from(
                theme
                    .ui_fg("error")
                    .unwrap_or(crate::tui::title_row::DEFAULT_ERROR_FG),
            );
            Some((format!("  {error}"), error_fg))
        }
        // With the palette up, the keys that walk it are worth naming once, after whatever
        // the line itself has to say.
        None => {
            let keys = palette
                .is_some()
                .then_some("Tab/Shift+Tab:Choose  Enter:Complete  Esc:Close");
            let hint = match (hint, keys) {
                (Some(hint), Some(keys)) => Some(format!("{hint} · {keys}")),
                (Some(hint), None) => Some(hint),
                (None, Some(keys)) => Some(format!("  {keys}")),
                (None, None) => None,
            };
            hint.map(|hint| {
                let weak = theme.color_for_scope("comment").unwrap_or(fg_rgb);
                (hint, TuiColor::from(weak))
            })
        }
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

    // The palette shares the bottom of the screen with the line, so it stays a glance: it
    // gives way before the input does, and never takes more than a third of the screen.
    let room = screen_rows
        .saturating_sub(rows)
        .min((screen_rows / 3).max(1));
    let list_rows = palette.as_ref().map_or(0, |palette| {
        palette.entries.len().min(MAX_LIST_ROWS).min(room)
    });
    let top = screen_rows.saturating_sub(rows + list_rows);

    let fill_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});
    let mut ops = RenderOpIRVec::new();
    for row_idx in top..(top + rows + list_rows).min(screen_rows) {
        ops += RenderOpCommon::MoveCursorPositionAbs(vp_col(0) + vp_row(row_idx as u16));
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
        vp_col(0) + vp_row(top as u16),
        width,
        rows,
        true,
        (color_bg, color_fg, color_selection),
    );
    let Some(palette) = palette else {
        pipeline.push(ZOrder::Normal, ops);
        return;
    };
    let picked_bg = TuiColor::from(crate::tui::title_row::title_selection_bg(false, theme));
    // Keep the highlighted row in view when the list has to be cut short.
    let first = (palette.selected + 1).saturating_sub(list_rows);
    let lines: Vec<(bool, String)> = palette
        .entries
        .iter()
        .enumerate()
        .skip(first)
        .take(list_rows)
        .map(|(index, entry)| {
            let picked = index == palette.selected;
            let marker = if picked { " > " } else { "   " };
            (
                picked,
                format!("{marker}{}   {}", entry.name(), entry.detail()),
            )
        })
        .collect();
    for (offset, (picked, line)) in lines.iter().enumerate() {
        let bg = if *picked { picked_bg } else { color_bg };
        let style = new_style!(color_fg: {color_fg} color_bg: {bg});
        let line = crate::tui::text_layout::truncate(line, width as usize);
        ops +=
            RenderOpCommon::MoveCursorPositionAbs(vp_col(0) + vp_row((top + rows + offset) as u16));
        ops += RenderOpCommon::ApplyColors(Some(style));
        ops += RenderOpIR::PaintTextWithAttributes(line.into(), Some(style));
    }
    pipeline.push(ZOrder::Normal, ops);
}

/// The terminal a `!` command opens: a plain shell when the command is empty.
pub fn shell_signal(
    shell_cmd: &str,
    file: Option<&camino::Utf8Path>,
    state: &crate::tui::state::AppState,
) -> Result<crate::tui::state::AppSignal, String> {
    use crate::tui::state::AppSignal;
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
fn find_crate_root(file_path: &camino::Utf8Path) -> Option<camino::Utf8PathBuf> {
    let mut dir = file_path.parent()?;
    loop {
        if dir.join("Cargo.toml").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Finds the nearest ancestor directory of `file_path` that contains a `.git` directory.
fn find_repo_root(file_path: &camino::Utf8Path) -> Option<camino::Utf8PathBuf> {
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
    file: Option<&camino::Utf8Path>,
    root: &camino::Utf8Path,
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
        let root = camino::Utf8Path::new("/repo");
        let file = camino::Utf8Path::new("/repo/src/main.rs");
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
        let root = camino::Utf8Path::new("/home/me/my repo");
        let file = camino::Utf8Path::new("/home/me/my repo/src/main.rs");
        assert_eq!(
            expand_placeholders("notify-send %project", None, root),
            Ok("notify-send 'my repo'".to_string())
        );
        assert_eq!(
            expand_placeholders("echo %project %", Some(file), root),
            Ok("echo 'my repo' 'src/main.rs'".to_string())
        );
    }

    fn alias(name: &str, command: &str) -> crate::config::Alias {
        use crate::config::Alias;
        Alias {
            name: name.to_string(),
            command: command.to_string(),
        }
    }

    fn state_with_aliases() -> crate::tui::state::AppState {
        use crate::tui::state::AppState;
        AppState {
            config_aliases: vec![
                alias("poweroff", "systemctl poweroff"),
                alias("notify", "notify-send %project"),
            ],
            project_aliases: vec![alias("notify", "echo project"), alias("ci", "cargo test")],
            ..crate::tui::state::AppState::default()
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
        set_alias(&mut state, "triple cargo nextest run").unwrap();
        assert_eq!(state.project_aliases.len(), 3);
        assert_eq!(state.project_aliases[2].command, "cargo nextest run");
        set_alias(&mut state, "triple cargo test").unwrap();
        assert_eq!(state.project_aliases.len(), 3);
        assert_eq!(state.project_aliases[2].command, "cargo test");
        set_alias(&mut state, "triple").unwrap();
        assert_eq!(state.project_aliases.len(), 2);
        assert!(state.session_dirty);
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
        assert_eq!(state.project_aliases.len(), 2);
    }

    #[test]
    fn hints_list_and_name_aliases() {
        let state = state_with_aliases();
        assert_eq!(
            command_hint("!", &state).unwrap(),
            " <shell command> · aliases: notify, ci, poweroff"
        );
        assert_eq!(
            command_hint("alias", &state).unwrap(),
            " notify = echo project · ci = cargo test · poweroff = systemctl poweroff (config)"
        );
        assert_eq!(
            command_hint("alias notify", &state).unwrap(),
            " Enter removes notify (was: echo project)"
        );
        assert_eq!(command_hint("ls", &state), None);
    }

    #[test]
    fn a_command_or_alias_typed_alone_says_what_it_does() {
        let state = state_with_aliases();
        assert_eq!(
            command_hint("scan", &state).as_deref(),
            Some(" read the repository for instructions aimed at an agent")
        );
        assert_eq!(
            command_hint("trust", &state).as_deref(),
            Some(" allow agent turns in this repository")
        );
        assert_eq!(
            command_hint("mcp", &state).as_deref(),
            Some(" Installed: none")
        );
        assert_eq!(
            command_hint("mcp add", &state).as_deref(),
            Some(" [name] [program]")
        );
        assert_eq!(
            command_hint("mcp remove", &state).as_deref(),
            Some(" (no server installed)")
        );
        assert_eq!(command_hint("mcp add egui", &state), None);
        assert_eq!(command_hint("ci", &state).as_deref(), Some(" = cargo test"));
        assert_eq!(
            command_hint("poweroff", &state).as_deref(),
            Some(" = systemctl poweroff (config)")
        );
        assert_eq!(
            command_hint("alias foo", &state).as_deref(),
            Some(" <shell command>")
        );
    }

    #[test]
    fn the_table_is_what_the_line_runs_and_the_palette_lists() {
        use crate::tui::state::AppState;
        // One table, so a command cannot be listed without being runnable, nor runnable
        // without being listed and described.
        let state = AppState::default();
        let listed: Vec<String> = palette_entries("", &state)
            .iter()
            .map(|entry| entry.name().to_string())
            .collect();
        for command in &COMMANDS {
            assert!(
                listed.iter().any(|name| name == command.name),
                "{} is not listed",
                command.name
            );
            assert!(
                !command.description.is_empty(),
                "{} has no description",
                command.name
            );
            assert!(
                command.argument.is_empty() || command.argument.starts_with('['),
                "{} spells its argument oddly: {}",
                command.name,
                command.argument
            );
        }
        let mut names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "two commands share a name");
    }

    #[test]
    fn palette_lists_commands_then_aliases_by_prefix() {
        let state = state_with_aliases();
        let names = |typed: &str| {
            palette_entries(typed, &state)
                .iter()
                .map(|entry| entry.name().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(""),
            [
                "after-agents",
                "alias",
                "all-features",
                "default-features",
                "features",
                "highlight",
                "mcp",
                "scan",
                "trust",
                "untrust",
                "notify",
                "ci",
                "poweroff"
            ]
        );
        assert_eq!(names("a"), ["after-agents", "alias", "all-features"]);
        assert_eq!(names("m"), ["mcp"]);
        assert_eq!(names("t"), ["trust"]);
        assert_eq!(names("c"), ["ci"]);
        assert_eq!(names("n"), ["notify"]);
        assert_eq!(names("p"), ["poweroff"]);
        assert!(names("alias ").is_empty());
        assert!(names("!").is_empty());
        assert!(names("nothing").is_empty());
    }

    #[test]
    fn a_word_naming_an_entry_exactly_hides_the_palette() {
        let state = state_with_aliases();
        for typed in [
            "alias",
            "scan",
            "trust",
            "untrust",
            "after-agents",
            "notify",
            "ci",
        ] {
            assert!(palette_entries(typed, &state).is_empty(), "{typed}");
        }
    }

    #[test]
    fn tab_and_enter_complete_to_the_highlighted_entry() {
        use crate::tui::input_line::InputLine;
        let state = state_with_aliases();
        let completed = |text: &str, selected: usize| {
            let mut line = CommandLine {
                input: InputLine::with_text(text),
                origin: None,
                error: None,
                selected,
            };
            line.accept(&palette_entries(text, &state));
            line.input.text().to_string()
        };
        assert_eq!(completed("", 0), "after-agents ");
        assert_eq!(completed("", 1), "alias ");
        assert_eq!(completed("", 5), "highlight ");
        assert_eq!(completed("", 6), "mcp ");
        assert_eq!(completed("", 10), "notify ");
        assert_eq!(completed("al", 0), "alias ");
        assert_eq!(completed("sc", 0), "scan");
        assert_eq!(completed("un", 0), "untrust");
        assert_eq!(completed("alias", 0), "alias");
    }

    #[test]
    fn moving_the_highlight_wraps_over_commands_and_aliases() {
        let state = state_with_aliases();
        let entries = palette_entries("", &state).len();
        assert_eq!(entries, 13);
        let mut line = CommandLine::new(None);
        assert!(line.move_selection(true, entries));
        assert_eq!(line.selected, 1);
        line.selected = 12;
        assert!(line.move_selection(true, entries));
        assert_eq!(line.selected, 0);
        assert!(line.move_selection(false, entries));
        assert_eq!(line.selected, 12);
        assert!(!line.move_selection(true, palette_entries("nothing", &state).len()));
    }

    #[test]
    fn server_names_are_unique_and_fit_tool_names() {
        use crate::agent::mcp::McpServerDecl;
        let servers = vec![McpServerDecl {
            name: "egui".to_string(),
            program: "egui-mcp".to_string(),
        }];
        assert!(check_server_name("doneward", &servers).is_ok());
        assert!(check_server_name("egui", &servers).is_err());
        assert!(check_server_name("a__b", &servers).is_err());
        assert!(check_server_name("a/b", &servers).is_err());
        assert!(check_server_name("egui.v2", &servers).is_err());
        assert!(check_server_name("egui-v2_", &servers).is_ok());
    }

    #[test]
    fn line_ranges_are_clamped_and_junk_is_skipped() {
        assert_eq!(parse_line_ranges("12", 100), Some(vec![(12, 12)]));
        assert_eq!(
            parse_line_ranges("1-3, 8 ,20-25", 100),
            Some(vec![(1, 3), (8, 8), (20, 25)])
        );
        assert_eq!(
            parse_line_ranges("9-4, 50-200", 60),
            Some(vec![(4, 9), (50, 60)])
        );
        assert_eq!(parse_line_ranges("0, 70, x, 3-", 60), None);
        assert_eq!(parse_line_ranges("x, 5", 60), Some(vec![(5, 5)]));
        assert_eq!(parse_line_ranges("3-7, 1", 0), None);
    }

    #[test]
    fn placeholder_without_a_file_is_refused() {
        let root = camino::Utf8Path::new("/repo");
        assert_eq!(
            expand_placeholders("cat %", None, root),
            Err("% needs a file preview in focus".to_string())
        );
        assert_eq!(expand_placeholders("ls", None, root), Ok("ls".to_string()));
    }
}
