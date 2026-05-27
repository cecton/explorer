use crate::state::R3blPaneState;
use rmux_sdk::{Pane, PaneEvent, PaneOutputChunk, PaneSnapshot};

pub struct R3blPaneDriver {
    pane: Pane,
    state: R3blPaneState,
}

impl R3blPaneDriver {
    pub fn new(pane: Pane) -> Self {
        Self {
            pane,
            state: R3blPaneState::from_snapshot(PaneSnapshot::default()),
        }
    }

    pub fn with_state(pane: Pane, state: R3blPaneState) -> Self {
        Self { pane, state }
    }

    pub fn pane(&self) -> &Pane {
        &self.pane
    }

    pub fn state(&self) -> &R3blPaneState {
        &self.state
    }

    pub fn state_snapshot(&self) -> R3blPaneState {
        self.state.clone()
    }

    pub fn state_mut(&mut self) -> &mut R3blPaneState {
        &mut self.state
    }

    pub async fn refresh(&mut self) -> rmux_sdk::Result<&R3blPaneState> {
        let snapshot = self.pane.snapshot().await?;
        self.apply_snapshot(snapshot);
        Ok(&self.state)
    }

    pub fn apply_snapshot(&mut self, snapshot: PaneSnapshot) {
        self.state.set_snapshot(snapshot);
    }

    pub fn apply_event(&mut self, event: &PaneEvent) {
        self.state.apply_event(event);
    }

    pub fn apply_output_chunk(&mut self, chunk: &PaneOutputChunk) {
        if let PaneOutputChunk::Lag(notice) = chunk {
            self.state.record_lag_notice(notice.clone());
        }
    }

    pub fn clear_lag(&mut self) {
        self.state.clear_lag();
    }
}
