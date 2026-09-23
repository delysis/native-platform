//! Window-lifetime decisions are observed through the real Tauri runtime.
use super::{
    NativeResult, lifecycle,
    surface::{Effect, Surface},
};
use crate::routing::SurfaceSlot;
use std::{sync::Arc, time::Instant};
use tauri::{EventLoopMessage, Manager};
use tauri_runtime::window::WindowId;
use tauri_runtime_wry::{
    Message, WindowIdStore, WindowMessage,
    tao::event::{Event, WindowEvent},
};

pub(super) enum Request {
    Close(Box<tauri::Window>),
    Exit,
    Release(Box<Surface>),
    #[cfg(target_os = "macos")]
    Focus(Box<tauri::Window>),
    #[cfg(target_os = "macos")]
    Notify(easl_native_accessibility::Notifications),
}
impl Request {
    pub fn send(
        self,
        proxy: &tauri_runtime_wry::tao::event_loop::EventLoopProxy<Message<EventLoopMessage>>,
    ) -> NativeResult<()> {
        match self {
            Self::Close(window) => window.close().map_err(|_| "managed-close-request"),
            Self::Release(surface) => {
                drop(surface);
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Self::Focus(window) => window
                .set_focus()
                .map_err(|_| "accessibility-focus-request"),
            #[cfg(target_os = "macos")]
            Self::Notify(notifications) => {
                notifications.raise();
                Ok(())
            }
            Self::Exit => proxy
                .send_event(Message::RequestExit(lifecycle::EXIT_CHECK_CODE))
                .map_err(|_| "managed-exit-request"),
        }
    }
}

pub(super) type Slot = SurfaceSlot<WindowId, Surface>;
pub(super) struct Host {
    slots: Vec<Slot>,
    check: Option<lifecycle::Check>,
}
impl Host {
    pub fn new(
        ids: Vec<WindowId>,
        windows: Vec<tauri::Window>,
        observations: Option<Arc<lifecycle::Observations>>,
        proxy: &tauri_runtime_wry::tao::event_loop::EventLoopProxy<Message<EventLoopMessage>>,
    ) -> NativeResult<Self> {
        if ids.len() != windows.len()
            || ids.is_empty()
            || ids.len() > 2
            || (ids.len() == 2 && ids[0] == ids[1])
        {
            return Err("managed-owner-inventory");
        }
        let app = windows[0].app_handle().clone();
        let mut slots = Vec::with_capacity(ids.len());
        for (index, (id, window)) in ids.into_iter().zip(windows).enumerate() {
            let mut surface = Surface::new(window, proxy.clone())?;
            if observations.is_some() {
                surface.seed_lifecycle_fixture(index)?;
            }
            let mut slot = SurfaceSlot::new(id, surface);
            slot.invalidate(&id);
            slots.push(slot);
        }
        Ok(Self {
            slots,
            check: observations.map(|data| lifecycle::Check::new(app, data)),
        })
    }
    fn slot(&mut self, id: WindowId) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|slot| *slot.owner() == id)
    }
    fn suspend(&mut self, id: WindowId) -> Vec<Request> {
        self.slot(id).and_then(suspend_slot).into_iter().collect()
    }
    pub fn suspend_all(&mut self) -> Vec<Request> {
        self.slots.iter_mut().filter_map(suspend_slot).collect()
    }
    fn destroyed(&mut self, id: WindowId) -> Vec<Request> {
        let statistics = self
            .slot(id)
            .and_then(|slot| slot.retained(&id))
            .map(|surface| {
                // Never turn emergency post-destruction cleanup into lifecycle proof.
                if surface.native_attached() {
                    super::reject("destroyed-with-native-resources-attached");
                }
                surface.statistics()
            });
        if let Some(statistics) = statistics
            && let Some(check) = &mut self.check
        {
            check.destroyed(id, statistics);
        }
        self.slot(id)
            .and_then(|slot| slot.take(&id))
            .map(|surface| Request::Release(Box::new(surface)))
            .into_iter()
            .collect()
    }
    pub fn check_deadline(&self) -> Option<Instant> {
        self.check.as_ref().map(lifecycle::Check::deadline)
    }
    pub fn check_expired(&self) -> bool {
        self.check_deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
    pub fn check_result(&self) -> NativeResult<Option<serde_json::Value>> {
        self.check
            .as_ref()
            .map(|check| check.finish(&self.slots))
            .transpose()
    }
    pub fn event(
        &mut self,
        event: &Event<'_, Message<EventLoopMessage>>,
        ids: &WindowIdStore,
    ) -> NativeResult<Vec<Request>> {
        match event {
            Event::WindowEvent {
                window_id, event, ..
            } => {
                let Some(id) = ids.get(window_id) else {
                    return Ok(Vec::new());
                };
                match event {
                    WindowEvent::CloseRequested => return Ok(self.suspend(id)),
                    WindowEvent::Destroyed => return Ok(self.destroyed(id)),
                    _ => return self.window_event(id, event),
                }
            }
            Event::UserEvent(Message::Window(
                id,
                WindowMessage::Close | WindowMessage::Destroy,
            )) => {
                return Ok(self.suspend(*id));
            }
            Event::UserEvent(Message::RequestExit(_)) | Event::LoopDestroyed => {
                return Ok(self.suspend_all());
            }
            Event::RedrawRequested(tao_id) => {
                if let Some(id) = ids.get(tao_id)
                    && let Some(surface) = self.slot(id).and_then(|slot| slot.redraw(&id))
                {
                    paint(surface)?;
                }
            }
            Event::MainEventsCleared => return self.finish_batch(),
            _ => {}
        }
        Ok(Vec::new())
    }
    fn window_event(
        &mut self,
        id: WindowId,
        event: &WindowEvent<'_>,
    ) -> NativeResult<Vec<Request>> {
        let Some(slot) = self.slot(id) else {
            return Ok(Vec::new());
        };
        let Some(surface) = slot.get_mut(&id) else {
            return Ok(Vec::new());
        };
        match surface.event(event)? {
            Effect::Redraw => {
                slot.invalidate(&id);
            }
            Effect::Close => return Ok(vec![Request::Close(Box::new(surface.window.clone()))]),
            Effect::None => {}
        }
        Ok(Vec::new())
    }
    fn finish_batch(&mut self) -> NativeResult<Vec<Request>> {
        let mut requests = Vec::new();
        for slot in &mut self.slots {
            let id = *slot.owner();
            if slot.is_suspended() {
                // This getter is dispatched by the already captured runtime ID.
                // A matching label in the Manager cannot revive an old native owner.
                let live = slot
                    .retained(&id)
                    .is_some_and(|s| s.window.inner_size().is_ok());
                if live && slot.resume(&id) {
                    let surface = slot.get_mut(&id).ok_or("resume-owner")?;
                    if let Err(stage) = surface.resume() {
                        surface.failed = true;
                        return Err(stage);
                    }
                }
            }
            if let Some(surface) = slot.get_mut(&id) {
                surface.finish_text_batch();
                #[cfg(target_os = "macos")]
                {
                    let (changed, focus) = surface.accessibility_actions()?;
                    if focus {
                        requests.push(Request::Focus(Box::new(surface.window.clone())));
                    }
                    if changed {
                        slot.invalidate(&id);
                    }
                }
            }
            // Public Tauri Window has no request_redraw. Flush one dirty frame
            // after the event batch; real OS exposures use the separate path above.
            if let Some(surface) = slot.take_redraw(&id) {
                paint(surface)?;
            }
            #[cfg(target_os = "macos")]
            if let Some(surface) = slot.get_mut(&id)
                && let Some(notifications) = surface.accessibility_update()?
            {
                requests.push(Request::Notify(notifications));
            }
        }
        if let Some(check) = &mut self.check {
            requests.extend(check.advance(&mut self.slots)?);
        }
        Ok(requests)
    }
}
fn suspend_slot(slot: &mut Slot) -> Option<Request> {
    let id = *slot.owner();
    #[cfg(target_os = "macos")]
    let mut notification = None;
    slot.suspend(&id, |surface| {
        surface.suspend();
        #[cfg(target_os = "macos")]
        {
            notification = surface.take_retiring_access().map(Request::Notify);
        }
    });
    #[cfg(target_os = "macos")]
    {
        notification
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
fn paint(surface: &mut Surface) -> NativeResult<()> {
    if let Err(stage) = surface.paint() {
        surface.failed = true;
        Err(stage)
    } else {
        Ok(())
    }
}
