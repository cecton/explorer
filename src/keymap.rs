//! Configurable top-level keyboard shortcuts (the leader + global keymap).
//!
//! Explorer's dispatch used to match each shortcut as a literal `KeyPress` pattern inline.
//! This module adds one level of indirection: an [`Action`] names *what* a binding does,
//! and a [`Keymap`] maps a [`KeyPress`] to an [`Action`] per context (leader vs global).
//! The dispatch in [`crate::tui`] resolves the key to an action, so the triggering key can
//! be overridden from `config.kdl` (see [`Keymap::apply_overrides`]).
//!
//! Config slugs accepted in the `keybindings` block: `leader`, `quit`, `focus-next`,
//! `focus-prev`, `open-window-picker`, `open-terminal`, `open-theme-picker`,
//! `open-git-log`, `open-conversations`, `toggle-favorite`, `open-model-picker`,
//! `close-pane`, `resize-grow`, `resize-shrink`, `move-pane-forward`, `move-pane-backward`,
//! `send-to-back`, `open-command-line`.

use r3bl_tui::{Key, KeyPress, KeyState, ModifierKeysMask, SpecialKey};

/// A semantic action a top-level key binding can trigger. `GrabTerminal` and
/// `DismissLeader` are intentionally not user-rebindable (they track the leader key / Esc),
/// so they are absent from [`Action::from_slug`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Quit,
    FocusNext,
    FocusPrev,
    OpenWindowPicker,
    OpenTerminal,
    OpenThemePicker,
    OpenGitLog,
    OpenConversations,
    ToggleFavorite,
    OpenModelPicker,
    ClosePane,
    GrabTerminal,
    DismissLeader,
    ResizeGrow,
    ResizeShrink,
    MovePaneForward,
    MovePaneBackward,
    SendToBack,
    OpenCommandLine,
}

impl Action {
    /// Resolves a config-file slug (e.g. `open-terminal`) to a rebindable action.
    fn from_slug(slug: &str) -> Option<Action> {
        Some(match slug {
            "quit" => Action::Quit,
            "focus-next" => Action::FocusNext,
            "focus-prev" => Action::FocusPrev,
            "open-window-picker" => Action::OpenWindowPicker,
            "open-terminal" => Action::OpenTerminal,
            "open-theme-picker" => Action::OpenThemePicker,
            "open-git-log" => Action::OpenGitLog,
            "open-conversations" => Action::OpenConversations,
            "toggle-favorite" => Action::ToggleFavorite,
            "open-model-picker" => Action::OpenModelPicker,
            "close-pane" => Action::ClosePane,
            "resize-grow" => Action::ResizeGrow,
            "resize-shrink" => Action::ResizeShrink,
            "move-pane-forward" => Action::MovePaneForward,
            "move-pane-backward" => Action::MovePaneBackward,
            "send-to-back" => Action::SendToBack,
            "open-command-line" => Action::OpenCommandLine,
            _ => return None,
        })
    }
}

/// The top-level keymap: the leader key plus a binding table for each context.
///
/// Stored as `Vec<(KeyPress, Action)>` (linear scan) rather than a `HashMap` because
/// `KeyPress` does not implement `Hash` and the tables have only a handful of entries.
#[derive(Clone, Debug)]
pub struct Keymap {
    /// Key that activates leader mode (default: `alt+\``). Pressing it again while a
    /// terminal pane is focused grabs that terminal ([`Action::GrabTerminal`]).
    pub leader_key: KeyPress,
    /// Bindings active while leader mode is engaged.
    pub leader: Vec<(KeyPress, Action)>,
    /// Bindings active outside leader mode.
    pub global: Vec<(KeyPress, Action)>,
}

impl Default for Keymap {
    /// The built-in bindings, matching explorer's historical hardcoded shortcuts.
    fn default() -> Self {
        Keymap {
            leader_key: alt_char('`'),
            leader: vec![
                (plain_char(' '), Action::OpenWindowPicker),
                (plain_char('t'), Action::OpenTerminal),
                (plain_char('T'), Action::OpenThemePicker),
                (plain_char('g'), Action::OpenGitLog),
                (plain_char('c'), Action::OpenConversations),
                (plain_char('f'), Action::ToggleFavorite),
                (plain_char('m'), Action::OpenModelPicker),
                (plain_char(':'), Action::OpenCommandLine),
                (plain_char('q'), Action::Quit),
                (plain_char('x'), Action::ClosePane),
                (plain_special(SpecialKey::Tab), Action::FocusNext),
                (plain_special(SpecialKey::BackTab), Action::FocusPrev),
                (plain_special(SpecialKey::Backspace), Action::SendToBack),
                (plain_special(SpecialKey::Esc), Action::DismissLeader),
            ],
            global: vec![
                (plain_special(SpecialKey::Tab), Action::FocusNext),
                (plain_special(SpecialKey::BackTab), Action::FocusPrev),
                (ctrl_special(SpecialKey::Down), Action::ResizeGrow),
                (ctrl_special(SpecialKey::Up), Action::ResizeShrink),
                (ctrl_special(SpecialKey::Left), Action::MovePaneForward),
                (ctrl_special(SpecialKey::Right), Action::MovePaneBackward),
            ],
        }
    }
}

impl Keymap {
    /// Resolves a key pressed while leader mode is engaged. The leader key itself maps to
    /// [`Action::GrabTerminal`] (the caller applies the "terminal focused" guard).
    pub fn leader_action(&self, kp: &KeyPress) -> Option<Action> {
        if *kp == self.leader_key {
            return Some(Action::GrabTerminal);
        }
        self.leader.iter().find(|(k, _)| k == kp).map(|(_, a)| *a)
    }

    /// Resolves a key pressed outside leader mode.
    pub fn global_action(&self, kp: &KeyPress) -> Option<Action> {
        self.global.iter().find(|(k, _)| k == kp).map(|(_, a)| *a)
    }

    /// The key bound to `action` in leader mode (for status-bar hints).
    pub fn leader_key_for(&self, action: Action) -> Option<KeyPress> {
        self.leader
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| *k)
    }

    /// The key bound to `action` outside leader mode (for status-bar hints).
    pub fn global_key_for(&self, action: Action) -> Option<KeyPress> {
        self.global
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| *k)
    }

    /// The label of two leader actions read together, so that a modifier they share is
    /// written once. An action that is unbound contributes nothing but its partner's label.
    pub fn leader_pair(&self, a: Action, b: Action) -> String {
        pair_of(self.leader_key_for(a), self.leader_key_for(b))
    }

    /// The label of two global actions read together, as in `Ctrl+↑/↓`.
    pub fn global_pair(&self, a: Action, b: Action) -> String {
        pair_of(self.global_key_for(a), self.global_key_for(b))
    }

    /// Applies `(action-slug, key-spec)` overrides from the config file on top of the
    /// defaults. The special slug `leader` rebinds [`Keymap::leader_key`]; every other slug
    /// rebinds its action in whichever context table(s) it appears (so `focus-next` updates
    /// both leader and global `Tab`).
    ///
    /// # Errors
    ///
    /// Returns a descriptive message on an unknown slug or an unparseable key spec.
    pub fn apply_overrides(&mut self, binds: &[(String, String)]) -> Result<(), String> {
        for (slug, spec) in binds {
            let key: KeyPress = spec
                .parse()
                .map_err(|e| format!("invalid key '{spec}' for '{slug}': {e}"))?;

            if slug == "leader" {
                self.leader_key = key;
                continue;
            }

            let action = Action::from_slug(slug)
                .ok_or_else(|| format!("unknown keybinding action '{slug}'"))?;

            for table in [&mut self.leader, &mut self.global] {
                for (k, a) in table.iter_mut() {
                    if *a == action {
                        *k = key;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Renders a [`KeyPress`] as a compact, human-friendly label for the status bar, e.g.
/// `Ctrl+↓`, `Shift+Tab`, `Alt+\``, `q`. Distinct from `KeyPress`'s `Display` (which
/// produces the lowercase config-file spelling like `ctrl+down`), so that `config.kdl`
/// keeps accepting the long name of a key however the status bar abbreviates it.
pub fn hint(kp: &KeyPress) -> String {
    let (key, mask) = match kp {
        KeyPress::Plain { key } => (key, None),
        KeyPress::WithModifiers { key, mask } => (key, Some(mask)),
    };
    let mut out = modifier_prefix(mask);
    match key {
        Key::Character(' ') => out.push_str("Space"),
        Key::Character(c) => out.push(*c),
        Key::SpecialKey(sk) => out.push_str(special_label(*sk)),
        Key::FunctionKey(fk) => out.push_str(&format!("F{}", u8::from(*fk))),
        Key::KittyKeyboardProtocol(_) => out.push('?'),
    }
    out
}

/// Renders two keys read together, writing a modifier they share once: `Ctrl+↑/↓`, never
/// `Ctrl+↑/Ctrl+↓`. Only whole modifier names are folded away, so keys spelled from the key
/// name up (`Tab` and `Shift+Tab`) keep both spellings, as do keys with no modifier in
/// common (`Ctrl+u/Alt+d`).
fn pair_hint(a: &KeyPress, b: &KeyPress) -> String {
    let (first, second) = (hint(a), hint(b));
    let shared = first
        .char_indices()
        .zip(second.chars())
        .take_while(|((_, x), y)| x == y)
        .last()
        .filter(|(_, c)| *c == '+')
        .map_or(0, |((index, _), _)| index + 1);
    format!("{first}/{}", &second[shared..])
}

/// [`pair_hint`] over actions that may be unbound: a lone key keeps its plain label,
/// neither renders empty.
fn pair_of(a: Option<KeyPress>, b: Option<KeyPress>) -> String {
    match (a, b) {
        (Some(a), Some(b)) => pair_hint(&a, &b),
        (Some(one), None) | (None, Some(one)) => hint(&one),
        (None, None) => String::new(),
    }
}

/// The modifier prefix a label starts with, in a fixed order: `Ctrl+`, `Alt+`, `Shift+`.
fn modifier_prefix(mask: Option<&ModifierKeysMask>) -> String {
    let mut out = String::new();
    if let Some(m) = mask {
        if m.ctrl_key_state == KeyState::Pressed {
            out.push_str("Ctrl+");
        }
        if m.alt_key_state == KeyState::Pressed {
            out.push_str("Alt+");
        }
        if m.shift_key_state == KeyState::Pressed {
            out.push_str("Shift+");
        }
    }
    out
}

fn special_label(sk: SpecialKey) -> &'static str {
    match sk {
        SpecialKey::Up => "↑",
        SpecialKey::Down => "↓",
        SpecialKey::Left => "←",
        SpecialKey::Right => "→",
        SpecialKey::Tab => "Tab",
        SpecialKey::BackTab => "Shift+Tab",
        SpecialKey::Enter => "Enter",
        SpecialKey::Esc => "Esc",
        SpecialKey::Backspace => "Bksp",
        SpecialKey::Delete => "Delete",
        SpecialKey::Insert => "Insert",
        SpecialKey::Home => "Home",
        SpecialKey::End => "End",
        SpecialKey::PageUp => "PgUp",
        SpecialKey::PageDown => "PgDn",
    }
}

fn plain_char(c: char) -> KeyPress {
    KeyPress::Plain {
        key: Key::Character(c),
    }
}

fn alt_char(c: char) -> KeyPress {
    KeyPress::WithModifiers {
        key: Key::Character(c),
        mask: ModifierKeysMask::new().with_alt(),
    }
}

fn plain_special(sk: SpecialKey) -> KeyPress {
    KeyPress::Plain {
        key: Key::SpecialKey(sk),
    }
}

fn ctrl_special(sk: SpecialKey) -> KeyPress {
    KeyPress::WithModifiers {
        key: Key::SpecialKey(sk),
        mask: ModifierKeysMask::new().with_ctrl(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_historical_bindings() {
        let km = Keymap::default();
        assert_eq!(km.leader_action(&plain_char('q')), Some(Action::Quit));
        assert_eq!(
            km.leader_action(&plain_char(' ')),
            Some(Action::OpenWindowPicker)
        );
        assert_eq!(
            km.leader_action(&plain_char('f')),
            Some(Action::ToggleFavorite)
        );
        assert_eq!(
            km.leader_action(&plain_char('m')),
            Some(Action::OpenModelPicker)
        );
        assert_eq!(
            km.leader_action(&plain_char(':')),
            Some(Action::OpenCommandLine)
        );
        assert_eq!(km.leader_action(&plain_char('z')), None);
        assert_eq!(
            km.leader_action(&plain_special(SpecialKey::Backspace)),
            Some(Action::SendToBack)
        );
        assert_eq!(
            km.global_action(&ctrl_special(SpecialKey::Down)),
            Some(Action::ResizeGrow)
        );
        assert_eq!(km.leader_action(&plain_char('g')), Some(Action::OpenGitLog));
        // Pressing the leader key inside leader mode grabs the terminal.
        assert_eq!(km.leader_action(&km.leader_key), Some(Action::GrabTerminal));
        assert_eq!(km.global_action(&plain_char('q')), None);
    }

    #[test]
    fn override_rebinds_action_in_all_contexts() {
        let mut km = Keymap::default();
        km.apply_overrides(&[("quit".to_string(), "Q".to_string())])
            .unwrap();
        assert_eq!(km.leader_action(&plain_char('Q')), Some(Action::Quit));
        assert_eq!(km.leader_action(&plain_char('q')), None);

        // focus-next lives in both tables; rebinding updates both.
        km.apply_overrides(&[("focus-next".to_string(), "ctrl+n".to_string())])
            .unwrap();
        let ctrl_n = "ctrl+n".parse::<KeyPress>().unwrap();
        assert_eq!(km.leader_action(&ctrl_n), Some(Action::FocusNext));
        assert_eq!(km.global_action(&ctrl_n), Some(Action::FocusNext));
    }

    #[test]
    fn override_can_rebind_leader_key() {
        let mut km = Keymap::default();
        km.apply_overrides(&[("leader".to_string(), "ctrl+b".to_string())])
            .unwrap();
        let ctrl_b = "ctrl+b".parse::<KeyPress>().unwrap();
        assert_eq!(km.leader_key, ctrl_b);
        assert_eq!(km.leader_action(&ctrl_b), Some(Action::GrabTerminal));
    }

    #[test]
    fn hint_labels_are_human_friendly() {
        assert_eq!(hint(&plain_char('z')), "z");
        assert_eq!(hint(&alt_char('`')), "Alt+`");
        assert_eq!(hint(&ctrl_special(SpecialKey::Down)), "Ctrl+↓");
        assert_eq!(hint(&plain_special(SpecialKey::BackTab)), "Shift+Tab");
        assert_eq!(hint(&plain_special(SpecialKey::Tab)), "Tab");
        assert_eq!(hint(&plain_special(SpecialKey::Backspace)), "Bksp");
    }

    #[test]
    fn a_shared_modifier_is_written_once_for_a_pair() {
        let mut km = Keymap::default();
        assert_eq!(
            km.global_pair(Action::FocusNext, Action::FocusPrev),
            "Tab/Shift+Tab"
        );
        assert_eq!(
            km.global_pair(Action::ResizeShrink, Action::ResizeGrow),
            "Ctrl+↑/↓"
        );
        assert_eq!(
            km.global_pair(Action::MovePaneForward, Action::MovePaneBackward),
            "Ctrl+←/→"
        );
        assert_eq!(
            km.leader_pair(Action::FocusNext, Action::FocusPrev),
            "Tab/Shift+Tab"
        );

        // A rebound pair folds whatever the two labels have in common.
        km.apply_overrides(&[
            ("resize-shrink".to_string(), "ctrl+u".to_string()),
            ("resize-grow".to_string(), "ctrl+d".to_string()),
        ])
        .unwrap();
        assert_eq!(
            km.global_pair(Action::ResizeShrink, Action::ResizeGrow),
            "Ctrl+u/d"
        );
        // Nothing in common above the key name: both are spelled in full.
        km.apply_overrides(&[("resize-grow".to_string(), "alt+d".to_string())])
            .unwrap();
        assert_eq!(
            km.global_pair(Action::ResizeShrink, Action::ResizeGrow),
            "Ctrl+u/Alt+d"
        );
    }

    #[test]
    fn a_pair_with_one_unbound_action_keeps_the_other_label() {
        let mut km = Keymap::default();
        km.global.clear();
        km.global
            .push((plain_special(SpecialKey::Tab), Action::FocusNext));
        assert_eq!(km.global_pair(Action::FocusNext, Action::FocusPrev), "Tab");
        assert_eq!(km.global_pair(Action::FocusPrev, Action::FocusNext), "Tab");
        assert_eq!(km.global_pair(Action::ResizeShrink, Action::ResizeGrow), "");
    }

    #[test]
    fn hints_follow_rebinds() {
        let mut km = Keymap::default();
        km.apply_overrides(&[
            ("quit".to_string(), "z".to_string()),
            ("leader".to_string(), "ctrl+b".to_string()),
        ])
        .unwrap();
        assert_eq!(
            km.leader_key_for(Action::Quit).map(|k| hint(&k)),
            Some("z".to_string())
        );
        assert_eq!(hint(&km.leader_key), "Ctrl+b");
    }

    #[test]
    fn override_rejects_bad_input() {
        let mut km = Keymap::default();
        assert!(
            km.apply_overrides(&[("quit".to_string(), "nonsense".to_string())])
                .is_err()
        );
        assert!(
            km.apply_overrides(&[("bogus-action".to_string(), "q".to_string())])
                .is_err()
        );
    }
}
