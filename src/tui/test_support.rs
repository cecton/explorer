//! Fixtures shared by the tests of the panes.

#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// An 80x24 `GlobalData` over `state`, drawing to a mock output device.
pub(crate) fn global_data(
    state: crate::tui::state::AppState,
) -> GlobalData<crate::tui::state::AppState, crate::signal::AppSignal> {
    let (main_tx, _) = tokio::sync::mpsc::channel(8);
    let size = vp_width(80) + vp_height(24);
    GlobalData::try_to_create_instance(
        main_tx,
        state,
        size,
        OutputDevice::new_mock().0,
        OfsBufPool::new(size),
    )
    .unwrap()
}
