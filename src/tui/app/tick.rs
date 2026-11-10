use std::time::Duration;

/// How many 5s `Housekeeping` calls apart a steady LSP health is logged: about 30s.
pub(super) const LSP_HEALTH_LOG_EVERY: u32 = 6;

/// Sends `signal` every `period`, the first time at once. A stall costs one late signal,
/// never a burst of catch-up ones; a full channel drops that period's signal.
pub(super) fn every(
    tx: &crate::signal::MainTx,
    period: Duration,
    signal: crate::signal::AppSignal,
) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            crate::signal::try_send(&tx, signal.clone());
        }
    });
}

/// Rechecks the working-tree dirty status off the main thread (it walks the index and
/// the worktree); the answer comes back as `DirtyStatusLoaded`.
pub(super) fn spawn_dirty_check(
    state: &crate::tui::state::AppState,
    main_tx: crate::signal::MainTx,
) {
    use crate::signal::AppSignal;
    let Some(repo) = state.repo.clone() else {
        return;
    };
    tokio::task::spawn_blocking(move || {
        let result = crate::git::is_dirty(&repo);
        crate::signal::send(&main_tx, AppSignal::DirtyStatusLoaded(result));
    });
}

/// The local minute since the epoch: the status-bar clock shows a new text when it changes.
pub(super) fn clock_minute() -> i64 {
    let now = crate::util::time::now_secs();
    (now + i64::from(crate::util::time::local_offset_secs(now))).div_euclid(60)
}

// save_session is intentionally a synchronous fast JSON write on the main thread.
// The 5s `Housekeeping` period is the debounce.
pub(super) fn maybe_save_session(state: &mut crate::tui::state::AppState, root: &camino::Utf8Path) {
    if !state.session_dirty {
        return;
    }
    match crate::tui::session::save_session(root, state) {
        Ok(()) => {
            state.session_dirty = false;
        }
        Err(e) => tracing::warn!("Failed to save session: {e}"),
    }
}
