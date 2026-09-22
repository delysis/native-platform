//! Pinned Wry-runtime event hook, not a second event loop or a replacement Runtime.
//! Deliberately one raw Tauri-owned window. Not installable in arbitrary applications.
mod keys;
mod surface;

use crate::routing::SurfaceSlot;
use std::{
    cell::{Cell, RefCell},
    error::Error,
    sync::Arc,
};
use surface::{Effect, Surface};
use tauri::EventLoopMessage;
use tauri_runtime_wry::{
    Context, EventLoopIterationContext, Message, Plugin, PluginBuilder, WebContextStore,
    WindowMessage,
    tao::{
        dpi::LogicalSize,
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoopProxy, EventLoopWindowTarget},
        window::{Window, WindowId},
    },
};

thread_local! {
    // The plugin is Send because it owns no native resource. All font, raster,
    // softbuffer and window references are constructed, used and dropped here on
    // the main event-loop thread; there is no unsafe Send/Sync implementation.
    static SURFACE: RefCell<Option<SurfaceSlot<WindowId, Surface>>> = const { RefCell::new(None) };
    static FAILURE: Cell<bool> = const { Cell::new(false) };
}

struct Hook;
impl PluginBuilder<EventLoopMessage> for Hook {
    type Plugin = Self;
    fn build(self, _: Context<EventLoopMessage>) -> Self {
        self
    }
}
impl Plugin<EventLoopMessage> for Hook {
    fn on_event(
        &mut self,
        event: &Event<'_, Message<EventLoopMessage>>,
        _: &EventLoopWindowTarget<Message<EventLoopMessage>>,
        proxy: &EventLoopProxy<Message<EventLoopMessage>>,
        _: &mut ControlFlow,
        context: EventLoopIterationContext<'_, EventLoopMessage>,
        _: &WebContextStore,
    ) -> bool {
        let redraw = SURFACE.with(|cell| {
            let Ok(mut borrowed) = cell.try_borrow_mut() else {
                reject("reentrant-native-event");
                return None;
            };
            let slot = borrowed.as_mut()?;
            let owner = *slot.owner();
            // This standalone probe has no close/exit vetoes. Release the last
            // renderer-held Arcs BEFORE Tauri closes its own raw window owner.
            let closing = match event {
                Event::WindowEvent {
                    window_id,
                    event: WindowEvent::CloseRequested | WindowEvent::Destroyed,
                    ..
                } => *window_id == owner,
                Event::UserEvent(Message::Window(
                    id,
                    WindowMessage::Close | WindowMessage::Destroy,
                )) => context.window_id_map.get(&owner).as_ref() == Some(id),
                Event::UserEvent(Message::RequestExit(_)) | Event::LoopDestroyed => true,
                _ => false,
            };
            if closing {
                if let Some(surface) = slot.get_mut(&owner) {
                    surface.finish_text_batch();
                }
                slot.close(&owner);
                return None;
            }
            let effect = match event {
                Event::WindowEvent {
                    window_id, event, ..
                } => {
                    let surface = slot.get_mut(window_id)?;
                    surface.event(event)
                }
                Event::RedrawRequested(window_id) => {
                    let surface = slot.redraw(window_id)?;
                    if let Err(stage) = surface.paint() {
                        surface.failed = true;
                        reject(stage);
                    }
                    return None;
                }
                Event::MainEventsCleared => {
                    slot.get_mut(&owner)?.finish_text_batch();
                    return None;
                }
                _ => return None,
            };
            match effect {
                Ok(Effect::Redraw) if slot.invalidate(&owner) => {
                    slot.get_mut(&owner).map(|s| s.window.clone())
                }
                Ok(Effect::Close) => {
                    if let Some(id) = context.window_id_map.get(&owner) {
                        if proxy
                            .send_event(Message::Window(id, WindowMessage::Close))
                            .is_err()
                        {
                            reject("close-request");
                        }
                    } else {
                        reject("close-owner-missing");
                    }
                    None
                }
                Err(stage) => {
                    reject(stage);
                    None
                }
                _ => None,
            }
        });
        // No mutable surface borrow survives an OS call that could schedule input.
        if let Some(window) = redraw {
            window.request_redraw();
        }
        // True would consume Tauri's lifecycle processing and later plugins.
        false
    }
}
fn reject(stage: &'static str) {
    FAILURE.with(|failure| failure.set(true));
    eprintln!(
        "{}",
        serde_json::json!({"schema":"delysis.easl-tauri-probe.failure.v1", "stage":stage, "qualified":false})
    );
}
fn clear() {
    SURFACE.with(|cell| {
        if let Ok(mut value) = cell.try_borrow_mut() {
            drop(value.take());
        } else {
            reject("cleanup-borrow");
        }
    });
}

/// This is a platform spike, not a product launcher. Its caller must require an
/// explicit user choice before invoking it. `build_info` does not construct it.
pub fn run(context: tauri::Context<tauri::Wry>) -> Result<(), Box<dyn Error>> {
    if !context.config().app.windows.is_empty() {
        return Err("Native probe forbids configured WebView windows".into());
    }
    let app = tauri::Builder::default()
        .enable_macos_default_menu(false)
        .setup(|app| {
            let empty = SURFACE.with(|cell| cell.borrow().is_none());
            if !empty {
                return Err("Only one native surface probe is supported".into());
            }
            app.wry_plugin(Hook);
            let weak = app.handle().create_tao_window(|| {
                (
                    "easl-native-probe".into(),
                    tauri_runtime_wry::TaoWindowBuilder::new()
                        .with_visible(false)
                        .with_title("EASL native surface — unqualified input probe")
                        .with_inner_size(LogicalSize::new(800., 600.))
                        .with_min_inner_size(LogicalSize::new(320., 240.)),
                )
            })?;
            let window: Arc<Window> = weak
                .upgrade()
                .ok_or("Native window closed before attachment")?;
            let surface = Surface::new(window.clone()).map_err(std::io::Error::other)?;
            SURFACE.with(|cell| *cell.borrow_mut() = Some(SurfaceSlot::new(window.id(), surface)));
            window.set_visible(true);
            window.request_redraw();
            Ok(())
        })
        .build(context)?;
    // App::run exits the process directly, skipping this function's postconditions.
    // The same Tauri loop must return so cleanup and failure reporting execute.
    let exit_code = app.run_return(|_, _| {});
    clear();
    if exit_code != 0 {
        return Err("Tauri event loop returned a failure status".into());
    }
    if FAILURE.with(Cell::get) {
        return Err("Native surface probe recorded a failure".into());
    }
    Ok(())
}

pub fn build_info() -> serde_json::Value {
    serde_json::json!({
        "schema":"delysis.easl-tauri-probe.build.v1",
        "reviewed_api_versions":{"tauri":"2.11.5", "tauri_runtime_wry":"2.11.4", "tao":"0.35.3"},
        "implementation":"raw-window runtime plugin + EASL layout/editing + native text/CPU raster",
        "webviews_requested":0, "webview_dependencies_removed":false,
        "data":"two ephemeral plain buffers; no project storage or model",
        "missing":["native IME/preedit and candidate rectangle binding", "OS accessibility adapter", "native acceptance"],
        "qualified":false
    })
}
