//! The rust-analyzer client: a process of its own on a thread of its own, driven by the
//! RRT framework, answering with colored previews and symbol highlights.

pub(crate) mod rpc;
pub(crate) mod symbols;
pub(crate) mod tokens;
pub(crate) mod worker;

use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub enum LspInput {
    /// Ask for the semantic tokens of the file at this index.
    TokenRequest(usize),
    /// Resolve the qualified name under this position and then every place it appears.
    HoverThenReferences {
        file_idx: usize,
        line: u32,
        character: u32,
        word: Option<String>,
        group_id: Option<usize>,
    },
    /// Restart rust-analyzer on another Cargo feature selection.
    Restart(crate::config::CargoFeatureSelection),
}

/// The interrupt the RRT engine reaches the worker with; the worker never has to be woken
/// early, since it polls the server's stdout and its own channel.
#[derive(Debug)]
pub struct LspInterrupt;

impl r3bl_tui::RRTSoftwareInterrupt for LspInterrupt {
    fn trigger_software_interrupt(&self) {}
}

pub static LSP_RRT: r3bl_tui::RRT<crate::lsp::worker::LspWorker> = r3bl_tui::RRT::new();

#[derive(Clone)]
pub struct LspConfig {
    pub root: camino::Utf8PathBuf,
    pub files: std::sync::Arc<arc_swap::ArcSwap<Vec<std::sync::Arc<crate::loader::LoadedFile>>>>,
    pub app_tx: crate::signal::MainTx,
    pub cargo_features: crate::config::CargoFeatureSelection,
}

impl std::fmt::Debug for LspConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspConfig")
            .field("root", &self.root)
            .field("cargo_features", &self.cargo_features)
            .finish()
    }
}

/// The `initializationOptions` that make rust-analyzer analyze `selection`. `Default`
/// omits the `cargo` key entirely, so rust-analyzer keeps its own default (the default
/// features only).
fn cargo_initialization_options(
    selection: &crate::config::CargoFeatureSelection,
) -> Option<serde_json::Value> {
    use crate::config::CargoFeatureSelection;
    match selection {
        CargoFeatureSelection::Default => None,
        CargoFeatureSelection::All => Some(serde_json::json!({
            "cargo": { "features": "all" }
        })),
        CargoFeatureSelection::Explicit {
            no_default_features,
            features,
        } => Some(serde_json::json!({
            "cargo": {
                "features": features,
                "noDefaultFeatures": no_default_features,
            }
        })),
    }
}

// ── Disabling the worker ─────────────────────────────────────────────────────

static DISABLED: OnceLock<bool> = OnceLock::new();

/// True when `EXPLORER_NO_LSP` asks for no rust-analyzer; read once per process.
pub fn disabled() -> bool {
    *DISABLED.get_or_init(|| {
        let value = std::env::var("EXPLORER_NO_LSP").unwrap_or_default();
        let value = value.trim();
        !(value.is_empty()
            || value == "0"
            || value.eq_ignore_ascii_case("false")
            || value.eq_ignore_ascii_case("off"))
    })
}

// ── Sending requests ─────────────────────────────────────────────────────────

/// Requests made while the worker was not running. The worker drains them itself as soon
/// as it dispatches, so nothing on the main thread has to retry them.
static REQUEST_BUFFER: parking_lot::Mutex<Vec<LspInput>> = parking_lot::const_mutex(Vec::new());

const MAX_BUFFERED_REQUESTS: usize = 10_000;

/// Takes what could not be sent yet, for the worker to send itself.
pub(crate) fn take_buffered_requests() -> Vec<LspInput> {
    std::mem::take(&mut *REQUEST_BUFFER.lock())
}

fn buffer_request(input: LspInput) {
    let mut buf = REQUEST_BUFFER.lock();
    if buf.len() >= MAX_BUFFERED_REQUESTS {
        tracing::warn!(
            "LSP: request buffer full ({} entries), dropping request",
            MAX_BUFFERED_REQUESTS
        );
    } else {
        buf.push(input);
    }
}

fn send_lsp_input(input: LspInput) {
    if disabled() {
        tracing::debug!("LSP: disabled, dropping input");
        return;
    }
    let generation = LSP_RRT.get_thread_generation();
    let guard = LSP_RRT.shared_state.lock();
    let r3bl_tui::core::ThreadState::Running(_, tx) = &*guard else {
        drop(guard);
        tracing::debug!("LSP: buffering input, the worker is not running (gen={generation})");
        buffer_request(input);
        return;
    };
    match tx.send(input.clone()) {
        Ok(0) => {
            tracing::warn!("LSP: send to 0 receivers (gen={generation})");
            buffer_request(input);
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!("LSP: send error: {:?} gen={}", e, generation);
            buffer_request(input);
        }
    }
}

pub fn send_file_request(file_idx: usize) {
    send_lsp_input(LspInput::TokenRequest(file_idx));
}

pub fn send_symbol_request(
    file_idx: usize,
    line: u32,
    character: u32,
    word: Option<String>,
    group_id: Option<usize>,
) {
    tracing::debug!(
        "LSP: send_symbol_request file_idx={} line={} char={} word={:?} group_id={:?}",
        file_idx,
        line,
        character,
        word,
        group_id,
    );
    send_lsp_input(LspInput::HoverThenReferences {
        file_idx,
        line,
        character,
        word,
        group_id,
    });
}

/// Restarts rust-analyzer on `selection`. Its `cargo.*` initialization options are only
/// read once, so nothing short of a fresh process applies a new feature selection.
pub fn set_cargo_features(selection: crate::config::CargoFeatureSelection) {
    send_lsp_input(LspInput::Restart(selection));
}

/// Health of the LSP worker, returned by [`health_check`].
#[derive(Debug)]
pub enum LspHealth {
    Running {
        input_receivers: usize,
        generation: u8,
    },
    NotRunning,
}

/// Returns the current health of the LSP worker.
pub fn health_check() -> LspHealth {
    let generation = LSP_RRT.get_thread_generation();
    let guard = LSP_RRT.shared_state.lock();
    match &*guard {
        r3bl_tui::core::ThreadState::Running(_, tx) => LspHealth::Running {
            input_receivers: tx.receiver_count(),
            generation,
        },
        _ => LspHealth::NotRunning,
    }
}

// ── Offsets ──────────────────────────────────────────────────────────────────

/// The byte offset of the UTF-16 offset `utf16_offset` of `s`, which is how the protocol
/// counts columns. Past the end is the end.
pub(crate) fn utf16_to_byte(s: &str, utf16_offset: usize) -> usize {
    let mut u16_count = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if u16_count >= utf16_offset {
            return byte_idx;
        }
        u16_count += ch.len_utf16();
    }
    s.len()
}

/// The UTF-16 offset of the byte offset `byte_offset` of `s`. Past the end is the end.
pub(crate) fn byte_to_utf16(s: &str, byte_offset: usize) -> u32 {
    let byte_offset = byte_offset.min(s.len());
    let mut u16_count = 0u32;
    for (i, c) in s.char_indices() {
        if byte_offset < i + c.len_utf8() {
            return u16_count;
        }
        u16_count += c.len_utf16() as u32;
    }
    u16_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CargoFeatureSelection;

    #[test]
    fn byte_to_utf16_ascii() {
        assert_eq!(byte_to_utf16("hello", 0), 0);
        assert_eq!(byte_to_utf16("hello", 3), 3);
        assert_eq!(byte_to_utf16("hello", 5), 5);
    }

    #[test]
    fn byte_to_utf16_multi_byte() {
        assert_eq!(byte_to_utf16("héllo", 0), 0);
        assert_eq!(byte_to_utf16("héllo", 2), 1);
        assert_eq!(byte_to_utf16("héllo", 3), 2);

        assert_eq!(byte_to_utf16("𐍈test", 0), 0);
        assert_eq!(byte_to_utf16("𐍈test", 4), 2);
        assert_eq!(byte_to_utf16("𐍈test", 5), 3);
    }

    #[test]
    fn byte_to_utf16_bounds() {
        assert_eq!(byte_to_utf16("hi", 100), 2);
    }

    /// A token's column comes back from the protocol as a UTF-16 offset, which
    /// `utf16_to_byte` turns into the byte offset a colored span is cut at.
    #[test]
    fn utf16_to_byte_round_trips_a_byte_offset() {
        for text in ["hello", "héllo", "𐍈test", "aé𐍈b"] {
            for (byte, _) in text.char_indices() {
                assert_eq!(
                    utf16_to_byte(text, byte_to_utf16(text, byte) as usize),
                    byte
                );
            }
            assert_eq!(
                utf16_to_byte(text, byte_to_utf16(text, text.len()) as usize),
                text.len()
            );
        }
    }

    #[test]
    fn utf16_to_byte_past_the_end_is_the_end() {
        assert_eq!(utf16_to_byte("hi", 9), 2);
    }

    #[test]
    fn cargo_init_options_default_omits_cargo_key() {
        assert_eq!(
            cargo_initialization_options(&CargoFeatureSelection::Default),
            None
        );
    }

    #[test]
    fn cargo_init_options_all_passes_all_features_string() {
        assert_eq!(
            cargo_initialization_options(&CargoFeatureSelection::All),
            Some(serde_json::json!({ "cargo": { "features": "all" } }))
        );
    }

    #[test]
    fn cargo_init_options_explicit_sets_features_and_no_default_features() {
        let selection = CargoFeatureSelection::Explicit {
            no_default_features: true,
            features: vec!["foo".to_string(), "bar".to_string()],
        };
        assert_eq!(
            cargo_initialization_options(&selection),
            Some(serde_json::json!({
                "cargo": {
                    "features": ["foo", "bar"],
                    "noDefaultFeatures": true,
                }
            }))
        );
    }

    #[test]
    fn cargo_init_options_explicit_no_default_only_sends_empty_feature_list() {
        let selection = CargoFeatureSelection::Explicit {
            no_default_features: true,
            features: vec![],
        };
        assert_eq!(
            cargo_initialization_options(&selection),
            Some(serde_json::json!({
                "cargo": {
                    "features": [],
                    "noDefaultFeatures": true,
                }
            }))
        );
    }
}
