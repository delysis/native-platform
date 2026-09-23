use super::keys::{self, Command};
use crate::TwoFields;
use easl_native_text::RasterSurface;
use easl_text::EditAction;
use std::num::NonZeroU32;
use tauri::Window;
use tauri_runtime_wry::tao::{
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::ModifiersState,
};

type Result<T> = std::result::Result<T, &'static str>;

// Only this bundle holds OS-dependent raster resources. A close intention can
// drop it without dropping the editor, its histories, policy VM or font context.
struct RenderTarget {
    pixels: softbuffer::Surface<Window, Window>,
    _context: softbuffer::Context<Window>,
}
impl RenderTarget {
    fn new(window: &Window) -> Result<Self> {
        let context = softbuffer::Context::new(window.clone()).map_err(|_| "surface-context")?;
        let pixels =
            softbuffer::Surface::new(&context, window.clone()).map_err(|_| "surface-buffer")?;
        Ok(Self {
            pixels,
            _context: context,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Statistics {
    pub frames: u64,
    pub presented_attachment: u64,
    pub attachments: u64,
    pub releases: u64,
    pub history_checks: u64,
    pub geometry_resolutions: u64,
}

pub(super) struct Surface {
    // Drop native buffers before the dispatcher handle. Unlike Arc<TaoWindow>,
    // a Tauri Window is not a second strong owner of the native window object.
    target: Option<RenderTarget>,
    // AccessKit retains its NSView independently of raster attachment. Preserve
    // this binding through close/exit vetoes; release only at actual destruction.
    #[cfg(target_os = "macos")]
    access: easl_native_accessibility::Bridge,
    #[cfg(target_os = "macos")]
    retiring_access: Option<easl_native_accessibility::Notifications>,
    #[cfg(target_os = "macos")]
    accessibility: crate::accessibility::Accessibility,
    pub window: Window,
    view: TwoFields,
    raster: Option<RasterSurface>,
    modifiers: ModifiersState,
    pointer: Option<[f32; 2]>,
    pointer_pending: bool,
    viewport: Option<[f32; 2]>,
    physical_size: tauri::PhysicalSize<u32>,
    scale: f64,
    geometry_dirty: bool,
    pub failed: bool,
    statistics: Statistics,
    text_events: crate::text_events::TextEvents,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Effect {
    None,
    Redraw,
    Close,
}
impl Surface {
    pub fn new(
        window: Window,
        proxy: tauri_runtime_wry::tao::event_loop::EventLoopProxy<
            tauri_runtime_wry::Message<tauri::EventLoopMessage>,
        >,
    ) -> Result<Self> {
        #[cfg(target_os = "macos")]
        let access = easl_native_accessibility::Bridge::attach(&window, move || {
            // Always enqueue. run_on_main_thread may execute inline, which would
            // not wake an idle event loop after an AX callback.
            proxy
                .send_event(tauri_runtime_wry::Message::Task(Box::new(|| {})))
                .is_ok()
        })
        .map_err(|_| "accessibility-attach")?;
        #[cfg(not(target_os = "macos"))]
        let _ = proxy;
        let target = Some(RenderTarget::new(&window)?);
        let mut this = Self {
            target,
            #[cfg(target_os = "macos")]
            access,
            #[cfg(target_os = "macos")]
            retiring_access: None,
            #[cfg(target_os = "macos")]
            accessibility: crate::accessibility::Accessibility::default(),
            window,
            view: TwoFields::new().map_err(|_| "editor-init")?,
            raster: None,
            modifiers: ModifiersState::empty(),
            pointer: None,
            pointer_pending: false,
            viewport: None,
            physical_size: tauri::PhysicalSize::new(0, 0),
            scale: 1.,
            geometry_dirty: true,
            failed: false,
            statistics: Statistics {
                attachments: 1,
                ..Statistics::default()
            },
            text_events: crate::text_events::TextEvents::default(),
        };
        this.resize()?;
        Ok(this)
    }
    pub fn statistics(&self) -> Statistics {
        self.statistics
    }
    pub fn native_attached(&self) -> bool {
        self.target.is_some()
    }
    pub fn suspend(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.retiring_access = Some(self.access.suspend());
        }
        self.finish_text_batch();
        if let Err(stage) = self.flush_pointer() {
            self.failed = true;
            super::reject(stage);
        }
        self.view.pointer_up();
        self.modifiers = ModifiersState::empty();
        self.pointer = None;
        if self.target.take().is_some() {
            self.statistics.releases = self.statistics.releases.saturating_add(1);
        }
    }
    #[cfg(target_os = "macos")]
    pub fn take_retiring_access(&mut self) -> Option<easl_native_accessibility::Notifications> {
        self.retiring_access.take()
    }
    pub fn resume(&mut self) -> Result<()> {
        // The host positively checked this same dispatcher's liveness after
        // Tauri's close/exit callbacks. Never run this after actual destruction.
        let focused = self.window.is_focused().map_err(|_| "resume-focus")?;
        self.view.set_window_focus(focused);
        if self.target.is_none() {
            self.target = Some(RenderTarget::new(&self.window)?);
            self.statistics.attachments = self.statistics.attachments.saturating_add(1);
        }
        self.geometry_dirty = true;
        #[cfg(target_os = "macos")]
        self.access.mailbox().set_accepting(true);
        Ok(())
    }
    fn scale(&self) -> Result<f64> {
        let scale = self.scale;
        if scale.is_finite() && (0.25..=8.).contains(&scale) {
            Ok(scale)
        } else {
            Err("display-scale")
        }
    }
    fn resize(&mut self) -> Result<bool> {
        if !self.geometry_dirty {
            return Ok(self.viewport.is_some());
        }
        // Managed-window getters cross a dispatcher. Resolve them only after a
        // geometry event, never on every pointer move/key or unchanged repaint.
        let scale = self.window.scale_factor().map_err(|_| "display-scale")?;
        if !scale.is_finite() || !(0.25..=8.).contains(&scale) {
            return Err("display-scale");
        }
        let physical = self.window.inner_size().map_err(|_| "window-size")?;
        // Also invalidate AX coordinates for scale-only changes and zero-size
        // transitions even when logical line wrapping happens to be unchanged.
        self.view.invalidate();
        let size = physical.to_logical::<f32>(scale);
        let next = [size.width, size.height];
        let drawable = size.width >= 64. && size.height >= 96.;
        if drawable {
            if self.viewport.map(|size| size.map(f32::to_bits)) != Some(next.map(f32::to_bits)) {
                self.view.resize(next).map_err(|_| "editor-layout")?;
            }
            self.viewport = Some(next);
        } else {
            self.viewport = None;
        }
        self.physical_size = physical;
        self.scale = scale;
        self.geometry_dirty = false;
        self.statistics.geometry_resolutions =
            self.statistics.geometry_resolutions.saturating_add(1);
        Ok(drawable)
    }
    pub fn event(&mut self, event: &WindowEvent<'_>) -> Result<Effect> {
        if self.failed {
            return Ok(Effect::None);
        }
        if let WindowEvent::ReceivedImeText(text) = event {
            self.text_events.receive(text, self.view.focused())?;
            return Ok(Effect::None);
        }
        let real_key = matches!(event, WindowEvent::KeyboardInput { event, is_synthetic: false, .. }
            if event.state == ElementState::Pressed);
        if !real_key {
            self.finish_text_batch();
        }
        match event {
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                // The hook runs BEFORE Tao/Tauri applies the event's new size.
                // Resolve window geometry at the next input/redraw, never here.
                self.geometry_dirty = true;
                self.pointer = None;
                self.pointer_pending = false;
                self.view.pointer_up();
                Ok(Effect::Redraw)
            }
            WindowEvent::Focused(focused) => {
                self.view.set_window_focus(*focused);
                if !focused {
                    self.modifiers = ModifiersState::empty();
                    self.pointer = None;
                    self.pointer_pending = false;
                }
                Ok(Effect::Redraw)
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = *modifiers;
                Ok(Effect::None)
            }
            _ if !self.view.focused() => Ok(Effect::None),
            _ if !self.resize()? => Ok(Effect::None),
            _ => self.input(event),
        }
    }
    fn input(&mut self, event: &WindowEvent<'_>) -> Result<Effect> {
        let redraw = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = position.to_logical::<f32>(self.scale()?);
                self.pointer = Some([point.x, point.y]);
                self.pointer_pending = self.view.capturing();
                self.pointer_pending
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if *state == ElementState::Pressed {
                    let Some(point) = self.pointer else {
                        return Ok(Effect::None);
                    };
                    self.view
                        .pointer_down(point, self.modifiers.shift_key())
                        .map_err(|_| "pointer-down")?
                } else {
                    let moved = self.flush_pointer()?;
                    self.view.pointer_up();
                    moved
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(point) = self.pointer else {
                    return Ok(Effect::None);
                };
                let delta = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 26.,
                    MouseScrollDelta::PixelDelta(point) => {
                        -point.to_logical::<f32>(self.scale()?).y
                    }
                    _ => 0.,
                };
                self.view.scroll(point, delta).map_err(|_| "scroll")?
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => return self.key(event),
            _ => false,
        };
        Ok(if redraw { Effect::Redraw } else { Effect::None })
    }
    fn flush_pointer(&mut self) -> Result<bool> {
        if !std::mem::take(&mut self.pointer_pending) {
            return Ok(false);
        }
        let Some(point) = self.pointer else {
            return Ok(false);
        };
        self.view.pointer_move(point).map_err(|_| "pointer-drag")
    }

    pub fn finish_text_batch(&mut self) {
        if let Err(stage) = self.text_events.barrier() {
            self.failed = true;
            super::reject(stage);
        }
    }
    #[cfg(target_os = "macos")]
    pub fn accessibility_actions(&mut self) -> Result<(bool, bool)> {
        self.access
            .mailbox()
            .begin_batch()
            .map_err(|_| "accessibility-callback")?;
        if self.failed || self.target.is_none() || !self.resize()? {
            self.access.mailbox().set_accepting(false);
            return Ok((false, false));
        }
        self.access.mailbox().set_accepting(true);
        let mut changed = false;
        let mut focus = false;
        for _ in 0..easl_native_accessibility::MAX_ACTIONS {
            let Some(action) = self
                .access
                .mailbox()
                .pop()
                .map_err(|_| "accessibility-callback")?
            else {
                break;
            };
            match self
                .accessibility
                .apply(&mut self.view, action.revision, &action.request)
                .map_err(|_| "accessibility-action")?
            {
                crate::accessibility::Outcome::Ignored => {}
                crate::accessibility::Outcome::Changed => changed = true,
                crate::accessibility::Outcome::FocusWindow => {
                    changed = true;
                    focus = true;
                }
            }
        }
        Ok((changed, focus))
    }
    #[cfg(target_os = "macos")]
    pub fn accessibility_update(
        &mut self,
    ) -> Result<Option<easl_native_accessibility::Notifications>> {
        if self.failed
            || self.viewport.is_none()
            || self.target.is_none()
            || !self
                .access
                .mailbox()
                .needs_update(self.view.presentation_revision())
        {
            return Ok(None);
        }
        let snapshot = self
            .accessibility
            .snapshot(&mut self.view, self.scale)
            .map_err(|_| "accessibility-tree")?;
        self.access
            .publish(
                snapshot.generation,
                snapshot.revision,
                snapshot.update,
                self.view.focused(),
            )
            .map(Some)
            .map_err(|_| "accessibility-publication")
    }
    fn key(&mut self, event: &KeyEvent) -> Result<Effect> {
        self.flush_pointer()?;
        let command = keys::resolve(
            &event.logical_key,
            event.text,
            self.modifiers,
            event.repeat,
            cfg!(target_os = "macos"),
        );
        let ordinary_text = match &command {
            Some(Command::Edit(EditAction::Replace(text))) => Some(*text),
            _ => None,
        };
        self.text_events.key(ordinary_text)?;
        let Some(command) = command else {
            return Ok(Effect::None);
        };
        match command {
            Command::Edit(action) => self.view.edit(action).map_err(|_| "edit")?,
            Command::Move(movement, extend) => self
                .view
                .move_caret(movement, extend)
                .map_err(|_| "navigation")?,
            Command::Undo(redo) => self.view.undo(redo).map_err(|_| "history")?,
            Command::FocusNext => self.view.cycle_focus(),
            Command::Copy(cut) => {
                let Some(text) = self.view.selected_text() else {
                    return Ok(Effect::None);
                };
                // A failed clipboard write must never delete the selected source.
                arboard::Clipboard::new()
                    .and_then(|mut c| c.set_text(text))
                    .map_err(|_| "clipboard-write")?;
                if cut {
                    self.view
                        .edit(EditAction::DeleteSelection)
                        .map_err(|_| "cut")?;
                }
            }
            Command::Paste => {
                let text = arboard::Clipboard::new()
                    .and_then(|mut c| c.get_text())
                    .map_err(|_| "clipboard-read")?;
                self.view
                    .edit(EditAction::Replace(&text))
                    .map_err(|_| "paste-admission")?;
            }
            Command::Close => return Ok(Effect::Close),
        }
        Ok(Effect::Redraw)
    }
    pub fn seed_lifecycle_fixture(&mut self, index: usize) -> Result<()> {
        let texts = fixture_text(index)?;
        self.view.set_window_focus(true);
        for text in texts {
            self.view
                .edit(EditAction::Replace(text))
                .map_err(|_| "fixture-edit")?;
            self.view.cycle_focus();
        }
        self.view.cycle_focus();
        self.view.set_window_focus(false);
        self.verify_lifecycle_fixture(index, false)
    }
    pub fn verify_lifecycle_fixture(&mut self, index: usize, check_undo: bool) -> Result<()> {
        let texts = fixture_text(index)?;
        verify_text(&self.view, texts)?;
        if check_undo {
            let focused = self.view.focused();
            self.view.set_window_focus(true);
            let result = (|| {
                self.view.undo(false).map_err(|_| "fixture-undo")?;
                if !self.view.text(1).map_err(|_| "fixture-read")?.is_empty()
                    || self.view.text(0).map_err(|_| "fixture-read")? != texts[0]
                {
                    return Err("fixture-undo-isolation");
                }
                self.view.undo(true).map_err(|_| "fixture-redo")?;
                verify_text(&self.view, texts)
            })();
            self.view.set_window_focus(focused);
            result?;
            self.statistics.history_checks = self.statistics.history_checks.saturating_add(1);
        }
        Ok(())
    }
    pub fn paint(&mut self) -> Result<()> {
        if self.failed || !self.resize()? {
            return Ok(());
        }
        self.flush_pointer()?;
        let size = self.physical_size;
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(());
        };
        if u64::from(size.width) * u64::from(size.height) > 16 * 1024 * 1024 {
            return Err("raster-limit");
        }
        let w = u16::try_from(size.width).map_err(|_| "raster-width")?;
        let h = u16::try_from(size.height).map_err(|_| "raster-height")?;
        let scale = self.scale()?;
        if self.raster.is_none() {
            self.raster = Some(RasterSurface::new(w, h, scale).map_err(|_| "raster-init")?);
        }
        let raster = self.raster.as_mut().ok_or("raster-missing")?;
        raster.begin(w, h, scale).map_err(|_| "raster-begin")?;
        raster
            .rect(
                [0., 0., f64::from(w) / scale, f64::from(h) / scale],
                [245, 245, 245, 255],
            )
            .map_err(|_| "raster-background")?;
        self.view.paint(raster).map_err(|_| "editor-paint")?;
        let rgba = raster.finish();
        let target = self.target.as_mut().ok_or("surface-suspended")?;
        target
            .pixels
            .resize(width, height)
            .map_err(|_| "surface-resize")?;
        let mut pixels = target.pixels.buffer_mut().map_err(|_| "surface-acquire")?;
        if pixels.len().checked_mul(4) != Some(rgba.len()) {
            return Err("raster-size");
        }
        for (pixel, color) in pixels.iter_mut().zip(rgba.chunks_exact(4)) {
            *pixel = (u32::from(color[0]) << 16) | (u32::from(color[1]) << 8) | u32::from(color[2]);
        }
        pixels.present().map_err(|_| "surface-present")?;
        self.statistics.frames = self.statistics.frames.saturating_add(1);
        self.statistics.presented_attachment = self.statistics.attachments;
        Ok(())
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        eprintln!(
            "{}",
            serde_json::json!({"schema":"delysis.easl-tauri-probe.shutdown.v1", "presented_frames":self.statistics.frames, "native_attachments":self.statistics.attachments, "native_releases":self.statistics.releases, "renderer_faulted":self.failed, "qualified":false})
        );
    }
}

fn fixture_text(index: usize) -> Result<[&'static str; 2]> {
    match index {
        0 => Ok(["first window café", "first window 日本語"]),
        1 => Ok(["second window café", "second window Ελληνικά"]),
        _ => Err("fixture-owner"),
    }
}
fn verify_text(view: &TwoFields, texts: [&str; 2]) -> Result<()> {
    if view.active() != 1 {
        return Err("fixture-focus-owner");
    }
    for (index, expected) in texts.into_iter().enumerate() {
        if view.text(index).map_err(|_| "fixture-read")? != expected
            || view.selection(index).map_err(|_| "fixture-selection")?
                != (expected.len(), expected.len())
        {
            return Err("fixture-source-or-selection");
        }
    }
    Ok(())
}
