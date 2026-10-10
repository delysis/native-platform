//! Winit adapter only. Logical geometry is independent of the window backend;
//! documents, focus, the scene and the renderer retain their existing owners.
use crate::{App, input_geometry, interface};
use std::time::{Duration, Instant};
use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, MouseScrollDelta},
    event_loop::ActiveEventLoop,
};

impl App {
    pub(crate) fn redraw(&mut self, loop_: &ActiveEventLoop) {
        if self.frame.pointer_pending {
            self.frame.pointer_pending = false;
            self.frame.dirty |= self.refresh_scene(if self.dragging { 5 } else { 0 }, loop_);
            if !self.frame.dirty {
                return;
            }
        }
        self.frame.dirty = false;
        if let Err(error) = self.paint() {
            self.error(error);
        }
    }

    pub(crate) fn cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        let scale = self
            .native
            .as_ref()
            .map_or(1., |native| native.window.scale_factor());
        let Some(point) = input_geometry::logical_point([position.x, position.y], scale) else {
            return;
        };
        self.pointer = point.map(interface::logical_pixels);
        // Retain every last position, but enqueue only one redraw for this
        // burst. Button release still flushes the final captured position.
        if self.frame.queue_pointer()
            && let Some(native) = &self.native
        {
            native.window.request_redraw();
        }
    }

    pub(crate) fn left_button(&mut self, state: ElementState, loop_: &ActiveEventLoop) {
        if state == ElementState::Pressed {
            self.focus_from_pointer();
            let now = Instant::now();
            let repeated = self.last_click.is_some_and(|(time, pos)| {
                now.duration_since(time) < Duration::from_millis(450)
                    && (pos[0] - self.pointer[0]).abs() < 4.
                    && (pos[1] - self.pointer[1]).abs() < 4.
            });
            self.clicks = if repeated { self.clicks % 3 + 1 } else { 1 };
            self.last_click = Some((now, self.pointer));
            self.update(1, loop_);
        } else {
            // A release can precede the queued redraw. Flush while ownership
            // still belongs to the drag, then release capture exactly once.
            if self.frame.pointer_pending && (self.dragging || self.divider.dragging()) {
                self.update(5, loop_);
            }
            self.dragging = false;
            self.divider.release();
            self.update_pointer_cursor();
        }
    }

    pub(crate) fn wheel(&mut self, delta: MouseScrollDelta, loop_: &ActiveEventLoop) {
        self.scroll = match delta {
            MouseScrollDelta::LineDelta(_, y) if y.is_finite() => -y,
            MouseScrollDelta::LineDelta(_, _) => return,
            MouseScrollDelta::PixelDelta(point) => {
                let scale = self
                    .native
                    .as_ref()
                    .map_or(1., |native| native.window.scale_factor());
                let Some(steps) = input_geometry::pixel_scroll_steps(point.y, scale) else {
                    return;
                };
                interface::logical_pixels(steps)
            }
        };
        self.update(3, loop_);
    }
}
