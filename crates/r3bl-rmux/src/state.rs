use rmux_sdk::{
    PaneDisconnectReason, PaneEvent, PaneExitReason, PaneId, PaneLagNotice, PaneSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PaneLifecycle {
    #[default]
    Live,
    Closed,
    Exited(PaneExitReason),
    Disconnected(PaneDisconnectReason),
}

impl PaneLifecycle {
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }
}

#[derive(Debug, Clone)]
pub struct R3blPaneState {
    pub pane_id: Option<PaneId>,
    pub snapshot: PaneSnapshot,
    pub lifecycle: PaneLifecycle,
    pub paused: bool,
    pub lagging: bool,
    pub last_lag_notice: Option<PaneLagNotice>,
    pub generation: u64,
}

impl R3blPaneState {
    pub fn from_snapshot(snapshot: PaneSnapshot) -> Self {
        Self {
            pane_id: None,
            snapshot,
            lifecycle: PaneLifecycle::Live,
            paused: false,
            lagging: false,
            last_lag_notice: None,
            generation: 0,
        }
    }

    pub fn cols(&self) -> u16 {
        self.snapshot.cols
    }

    pub fn rows(&self) -> u16 {
        self.snapshot.rows
    }

    pub fn revision(&self) -> u64 {
        self.snapshot.revision
    }

    pub fn set_snapshot(&mut self, snapshot: PaneSnapshot) {
        if snapshot.revision != self.snapshot.revision {
            self.generation = self.generation.wrapping_add(1);
        }
        self.snapshot = snapshot;
    }

    pub fn set_pane_id(&mut self, pane_id: PaneId) {
        self.pane_id = Some(pane_id);
    }

    pub fn clear_lag(&mut self) {
        self.lagging = false;
        self.last_lag_notice = None;
    }

    pub fn record_lag_notice(&mut self, notice: PaneLagNotice) {
        self.lagging = true;
        self.last_lag_notice = Some(notice);
    }

    pub fn apply_event(&mut self, event: &PaneEvent) {
        match event {
            PaneEvent::Close { .. } => {
                self.lifecycle = PaneLifecycle::Closed;
            }
            PaneEvent::Exit { reason } => {
                self.lifecycle = PaneLifecycle::Exited(reason.clone());
            }
            PaneEvent::Disconnect { reason, .. } => {
                self.lifecycle = PaneLifecycle::Disconnected(reason.clone());
            }
            PaneEvent::Pause { .. } => {
                self.paused = true;
            }
            PaneEvent::Continue { .. } => {
                self.paused = false;
            }
            PaneEvent::Lag { .. } => {
                self.lagging = true;
            }
            _ => {}
        }
    }
}
