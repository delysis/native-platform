use super::keys::{self, Command};
use crate::TwoFields;
use easl_native_text::RasterSurface;
use easl_text::EditAction;
use std::{num::NonZeroU32, sync::Arc};
use tauri_runtime_wry::tao::{
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::ModifiersState,
    window::Window,
};

type Result<T> = std::result::Result<T, &'static str>;

pub(super) struct Surface {
    // Declaration order deliberately drops native buffers before context/window.
    pixels: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    _context: softbuffer::Context<Arc<Window>>,
    pub window: Arc<Window>,
    view: TwoFields,
    raster: Option<RasterSurface>,
    modifiers: ModifiersState,
    pointer: Option<[f32; 2]>,
    pointer_pending: bool,
    viewport: Option<[f32; 2]>,
    pub failed: bool,
    frames: u64,
    text_events: crate::text_events::TextEvents,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Effect {
    None,
    Redraw,
    Close,
}
impl Surface {
    pub fn new(window: Arc<Window>) -> Result<Self> {
        let context = softbuffer::Context::new(window.clone()).map_err(|_| "surface-context")?;
        let pixels =
            softbuffer::Surface::new(&context, window.clone()).map_err(|_| "surface-buffer")?;
        let mut this = Self {
            pixels,
            _context: context,
            window,
            view: TwoFields::new().map_err(|_| "editor-init")?,
            raster: None,
            modifiers: ModifiersState::empty(),
            pointer: None,
            pointer_pending: false,
            viewport: None,
            failed: false,
            frames: 0,
            text_events: crate::text_events::TextEvents::default(),
        };
        this.resize()?;
        Ok(this)
    }
    fn scale(&self) -> Result<f64> {
        let scale = self.window.scale_factor();
        if scale.is_finite() && (0.25..=8.).contains(&scale) {
            Ok(scale)
        } else {
            Err("display-scale")
        }
    }
    fn resize(&mut self) -> Result<bool> {
        let size = self.window.inner_size().to_logical::<f32>(self.scale()?);
        if size.width < 64. || size.height < 96. {
            self.viewport = None;
            return Ok(false);
        }
        let next = [size.width, size.height];
        if self.viewport.map(|size| size.map(f32::to_bits)) != Some(next.map(f32::to_bits)) {
            self.view.resize(next).map_err(|_| "editor-layout")?;
            self.viewport = Some(next);
        }
        Ok(true)
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
            super::reject(stage);
        }
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
    pub fn paint(&mut self) -> Result<()> {
        if self.failed || !self.resize()? {
            return Ok(());
        }
        self.flush_pointer()?;
        let size = self.window.inner_size();
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
        self.pixels
            .resize(width, height)
            .map_err(|_| "surface-resize")?;
        let mut pixels = self.pixels.buffer_mut().map_err(|_| "surface-acquire")?;
        if pixels.len().checked_mul(4) != Some(rgba.len()) {
            return Err("raster-size");
        }
        for (pixel, color) in pixels.iter_mut().zip(rgba.chunks_exact(4)) {
            *pixel = (u32::from(color[0]) << 16) | (u32::from(color[1]) << 8) | u32::from(color[2]);
        }
        pixels.present().map_err(|_| "surface-present")?;
        self.frames = self.frames.saturating_add(1);
        Ok(())
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        eprintln!(
            "{}",
            serde_json::json!({"schema":"delysis.easl-tauri-probe.shutdown.v1", "presented_frames":self.frames, "renderer_faulted":self.failed, "qualified":false})
        );
    }
}
