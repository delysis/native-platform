//! Opt-in native lifecycle acceptance, not a synthetic UI-success signal.
use super::{
    LABELS, NativeResult,
    host::{Request, Slot},
    surface::Statistics,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::Manager;
use tauri_runtime::window::WindowId;

pub(super) const EXIT_CHECK_CODE: i32 = 73;

#[derive(Default)]
pub(super) struct Observations {
    closes: [AtomicUsize; 2],
    destroyed: [AtomicBool; 2],
    exits: AtomicUsize,
    manager_removed: [AtomicBool; 2],
}
impl Observations {
    fn veto_close(&self, index: usize) -> bool {
        let previous = self.closes[index].fetch_add(1, Ordering::SeqCst);
        index == 0 && previous == 0
    }
    fn veto_exit(&self, code: Option<i32>) -> bool {
        code == Some(EXIT_CHECK_CODE) && self.exits.fetch_add(1, Ordering::SeqCst) == 0
    }
    pub fn listen(self: &Arc<Self>, window: &tauri::Window, index: usize) {
        let data = Arc::clone(self);
        window.on_window_event(move |event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if data.veto_close(index) {
                    api.prevent_close();
                }
            }
            tauri::WindowEvent::Destroyed => data.destroyed[index].store(true, Ordering::SeqCst),
            _ => {}
        });
    }
    pub fn run_event(&self, app: &tauri::AppHandle, event: &tauri::RunEvent) {
        if let tauri::RunEvent::ExitRequested { code, api, .. } = event
            && self.veto_exit(*code)
        {
            api.prevent_exit();
        }
        // Record Manager state before final cleanup. No Tauri getter is called
        // after run_return, when cleanup_before_exit has already run.
        if matches!(event, tauri::RunEvent::ExitRequested { code: None, .. })
            && self.destroyed(0)
            && self.destroyed(1)
        {
            for (index, label) in LABELS.into_iter().enumerate() {
                self.manager_removed[index].store(
                    app.get_window(label).is_none() && app.get_webview(label).is_none(),
                    Ordering::SeqCst,
                );
            }
        }
    }
    fn closes(&self, index: usize) -> usize {
        self.closes[index].load(Ordering::SeqCst)
    }
    fn destroyed(&self, index: usize) -> bool {
        self.destroyed[index].load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    InitialPaint,
    CloseVeto,
    ExitVeto,
    PrimaryDestruction,
    PeerDestruction,
}

pub(super) struct Check {
    app: tauri::AppHandle,
    observations: Arc<Observations>,
    phase: Phase,
    deadline: Instant,
    close_veto_verified: bool,
    exit_veto_verified: bool,
    closed: Vec<(WindowId, Statistics)>,
}
impl Check {
    pub fn new(app: tauri::AppHandle, observations: Arc<Observations>) -> Self {
        Self {
            app,
            observations,
            phase: Phase::InitialPaint,
            deadline: Instant::now() + Duration::from_secs(15),
            close_veto_verified: false,
            exit_veto_verified: false,
            closed: Vec::with_capacity(2),
        }
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn destroyed(&mut self, id: WindowId, statistics: Statistics) {
        if self.closed.len() >= 2 || self.closed.iter().any(|(previous, _)| *previous == id) {
            super::reject("lifecycle-duplicate-destruction");
        } else {
            self.closed.push((id, statistics));
        }
    }
    pub fn advance(&mut self, slots: &mut [Slot]) -> NativeResult<Vec<Request>> {
        if slots.len() != 2 {
            return Err("lifecycle-window-count");
        }
        for slot in slots.iter() {
            if let Some(surface) = slot.retained(slot.owner())
                && surface.window.is_visible().is_ok_and(|visible| visible)
            {
                return Err("lifecycle-window-became-visible");
            }
        }
        match self.phase {
            Phase::InitialPaint => {
                verify_slots(slots, [1, 1], [0, 0], false)?;
                self.phase = Phase::CloseVeto;
                close_request(&slots[0])
            }
            Phase::CloseVeto => self.after_close_veto(slots),
            Phase::ExitVeto => self.after_exit_veto(slots),
            Phase::PrimaryDestruction => self.after_primary_destruction(slots),
            Phase::PeerDestruction => Ok(Vec::new()),
        }
    }
    fn after_close_veto(&mut self, slots: &mut [Slot]) -> NativeResult<Vec<Request>> {
        if self.observations.closes(0) == 0 {
            return Ok(Vec::new());
        }
        if self.observations.closes(0) != 1
            || self.observations.closes(1) != 0
            || self.observations.destroyed(0)
        {
            return Err("lifecycle-close-veto-not-retained");
        }
        verify_slots(slots, [2, 1], [1, 0], true)?;
        self.close_veto_verified = true;
        self.phase = Phase::ExitVeto;
        Ok(vec![Request::Exit])
    }
    fn after_exit_veto(&mut self, slots: &mut [Slot]) -> NativeResult<Vec<Request>> {
        if self.observations.exits.load(Ordering::SeqCst) == 0 {
            return Ok(Vec::new());
        }
        if self.observations.exits.load(Ordering::SeqCst) != 1
            || self.observations.destroyed(0)
            || self.observations.destroyed(1)
        {
            return Err("lifecycle-exit-veto-not-retained");
        }
        verify_slots(slots, [3, 2], [2, 1], true)?;
        self.exit_veto_verified = true;
        self.phase = Phase::PrimaryDestruction;
        close_request(&slots[0])
    }
    fn after_primary_destruction(&mut self, slots: &mut [Slot]) -> NativeResult<Vec<Request>> {
        if !self.observations.destroyed(0) {
            return Ok(Vec::new());
        }
        if slots[0].retained(slots[0].owner()).is_some()
            || self.app.get_window(LABELS[0]).is_some()
            || self.observations.destroyed(1)
            || self.observations.closes(0) != 2
        {
            return Err("lifecycle-primary-destruction");
        }
        let peer_id = *slots[1].owner();
        let peer = slots[1].get_mut(&peer_id).ok_or("lifecycle-peer-missing")?;
        peer.verify_lifecycle_fixture(1, false)?;
        let stats = peer.statistics();
        if !peer.native_attached() || stats.attachments != 2 || stats.releases != 1 {
            return Err("lifecycle-peer-affected-by-foreign-close");
        }
        self.phase = Phase::PeerDestruction;
        close_request(&slots[1])
    }
    pub fn finish(&self, slots: &[Slot]) -> NativeResult<serde_json::Value> {
        if self.phase != Phase::PeerDestruction
            || !self.close_veto_verified
            || !self.exit_veto_verified
            || slots.len() != 2
            || self.closed.len() != 2
            || self.observations.closes(0) != 2
            || self.observations.closes(1) != 1
            || self.observations.exits.load(Ordering::SeqCst) != 1
        {
            return Err("incomplete-native-lifecycle-check");
        }
        let mut windows = Vec::with_capacity(2);
        for (index, slot) in slots.iter().enumerate() {
            if !self.observations.destroyed(index)
                || slot.retained(slot.owner()).is_some()
                || !self.observations.manager_removed[index].load(Ordering::SeqCst)
            {
                return Err("native-owner-survived-destruction");
            }
            let stats = self
                .closed
                .iter()
                .find(|(id, _)| id == slot.owner())
                .map(|(_, stats)| stats)
                .ok_or("native-destruction-receipt-missing")?;
            let expected = if index == 0 { 3 } else { 2 };
            if stats.frames == 0
                || stats.presented_attachment != expected
                || stats.attachments != expected
                || stats.releases != expected
                || stats.history_checks != 2
            {
                return Err("native-resource-release-count");
            }
            windows.push(serde_json::json!({
                "window_index":index, "presented_frames":stats.frames,
                "native_attachments":stats.attachments, "native_releases":stats.releases,
                "geometry_resolutions":stats.geometry_resolutions,
                "source_selection_and_undo_checks":stats.history_checks,
                "manager_removed":true, "destroyed_callback_observed":true
            }));
        }
        Ok(serde_json::json!({
            "schema":"delysis.easl-tauri-managed-lifecycle.v1",
            "evidence_class":"hidden-native-window-lifecycle", "windows":windows,
            "close_veto_retained_editors":true, "exit_veto_retained_editors":true,
            "peer_survived_foreign_close":true, "webviews_requested":0,
            "source":"fixed ephemeral fixtures; no user files or clipboard",
            "visual_acceptance":false, "ime_acceptance":false,
            "accessibility_acceptance":false, "qualified":false
        }))
    }
}

fn close_request(slot: &Slot) -> NativeResult<Vec<Request>> {
    let surface = slot.retained(slot.owner()).ok_or("lifecycle-close-owner")?;
    Ok(vec![Request::Close(Box::new(surface.window.clone()))])
}
fn verify_slots(
    slots: &mut [Slot],
    attachments: [u64; 2],
    releases: [u64; 2],
    undo: bool,
) -> NativeResult<()> {
    for (index, slot) in slots.iter_mut().enumerate() {
        let id = *slot.owner();
        let surface = slot.get_mut(&id).ok_or("lifecycle-live-owner")?;
        if surface
            .window
            .is_visible()
            .map_err(|_| "lifecycle-visibility")?
        {
            return Err("lifecycle-window-visible");
        }
        let stats = surface.statistics();
        if !surface.native_attached()
            || stats.frames == 0
            || stats.presented_attachment != stats.attachments
            || stats.attachments != attachments[index]
            || stats.releases != releases[index]
        {
            return Err("lifecycle-native-resource-transition");
        }
        surface.verify_lifecycle_fixture(index, undo)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_listener_vetoes_only_the_first_primary_close() {
        let data = Observations::default();
        assert!(data.veto_close(0));
        assert!(!data.veto_close(0));
        assert!(!data.veto_close(1));
        assert_eq!(data.closes(0), 2);
        assert_eq!(data.closes(1), 1);
    }
    #[test]
    fn ordinary_exit_and_wrong_exit_code_cannot_satisfy_the_explicit_exit_probe() {
        let data = Observations::default();
        assert!(!data.veto_exit(None));
        assert!(!data.veto_exit(Some(0)));
        assert_eq!(data.exits.load(Ordering::SeqCst), 0);
        assert!(data.veto_exit(Some(EXIT_CHECK_CODE)));
        assert!(!data.veto_exit(Some(EXIT_CHECK_CODE)));
        assert_eq!(data.exits.load(Ordering::SeqCst), 2);
    }
}
