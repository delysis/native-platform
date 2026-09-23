//! Pinned native-event bridge using real Tauri-managed, webview-free windows.
//! Close intentions release OS resources; only destruction releases the editor.
mod host;
mod keys;
mod lifecycle;
mod surface;

use host::Host;
use std::{
    cell::{Cell, RefCell},
    error::Error,
    sync::Arc,
};
use tauri::{EventLoopMessage, Manager};
use tauri_runtime_wry::{
    Context, EventLoopIterationContext, Message, Plugin, PluginBuilder, WebContextStore,
    tao::{
        event::Event,
        event_loop::{ControlFlow, EventLoopProxy, EventLoopWindowTarget},
    },
};

const LABELS: [&str; 2] = ["easl-native-probe", "easl-native-probe-peer"];
type NativeResult<T> = Result<T, &'static str>;

thread_local! {
    // Native objects never cross threads. The Send plugin has no renderer,
    // font or OS-window resource and no unsafe Send/Sync implementation.
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    static FAILURE: Cell<bool> = const { Cell::new(false) };
}

struct HookBuilder {
    windows: Vec<tauri::Window>,
    observations: Option<Arc<lifecycle::Observations>>,
}
struct Hook;
impl PluginBuilder<EventLoopMessage> for HookBuilder {
    type Plugin = Hook;
    fn build(self, context: Context<EventLoopMessage>) -> Hook {
        let proxy = context.proxy.clone();
        let result = context.run_threaded(|main| {
            let main = main.ok_or("attachment-not-on-main-thread")?;
            // Labels resolve only during attachment. Later routing uses the exact
            // numeric runtime ID; an unrelated/reused label cannot claim a view.
            let ids = {
                let windows = main
                    .windows
                    .0
                    .try_borrow()
                    .map_err(|_| "runtime-window-borrow")?;
                self.windows
                    .iter()
                    .map(|window| {
                        let mut matches = windows
                            .iter()
                            .filter(|(_, item)| item.label() == window.label());
                        let (&id, _) = matches.next().ok_or("managed-window-missing")?;
                        if matches.next().is_some() {
                            return Err("managed-window-ambiguous");
                        }
                        Ok(id)
                    })
                    .collect::<NativeResult<Vec<_>>>()?
            };
            // No runtime window-store borrow survives native surface creation.
            let host = Host::new(ids, self.windows, self.observations, &proxy)?;
            HOST.with(|cell| {
                let mut slot = cell.try_borrow_mut().map_err(|_| "attachment-borrow")?;
                if slot.is_some() {
                    return Err("native-hook-already-installed");
                }
                *slot = Some(host);
                Ok(())
            })
        });
        if let Err(stage) = result {
            reject(stage);
        }
        Hook
    }
}
impl Plugin<EventLoopMessage> for Hook {
    fn on_event(
        &mut self,
        event: &Event<'_, Message<EventLoopMessage>>,
        _: &EventLoopWindowTarget<Message<EventLoopMessage>>,
        proxy: &EventLoopProxy<Message<EventLoopMessage>>,
        control_flow: &mut ControlFlow,
        context: EventLoopIterationContext<'_, EventLoopMessage>,
        _: &WebContextStore,
    ) -> bool {
        let result = HOST.with(|cell| {
            let mut borrowed = cell
                .try_borrow_mut()
                .map_err(|_| "reentrant-native-event")?;
            let Some(host) = borrowed.as_mut() else {
                return Ok(Vec::new());
            };
            if host.check_expired() {
                let requests = host.suspend_all();
                *control_flow = ControlFlow::Exit;
                reject("lifecycle-check-timeout");
                return Ok(requests);
            }
            let requests = host.event(event, &context.window_id_map)?;
            if let Some(deadline) = host.check_deadline() {
                // Only the explicit hidden-window checker has a deadline.
                // Interactive use does not poll or change Tauri's control flow.
                if !matches!(*control_flow, ControlFlow::ExitWithCode(_)) {
                    *control_flow = ControlFlow::WaitUntil(deadline);
                }
            }
            Ok(requests)
        });
        match result {
            Ok(requests) => {
                // Application callbacks/close requests never run under HOST's borrow.
                for request in requests {
                    if let Err(stage) = request.send(proxy) {
                        reject(stage);
                    }
                }
            }
            Err(stage) => reject(stage),
        }
        // Never consume lifecycle events or bypass Tauri/plugin close/exit vetoes.
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
    let host = HOST.with(|cell| {
        if let Ok(mut value) = cell.try_borrow_mut() {
            value.take()
        } else {
            reject("cleanup-borrow");
            None
        }
    });
    if let Some(mut host) = host {
        // Retire native AX children, then release the adapter outside HOST's borrow.
        // Reentrant callbacks see no editor owner during final cleanup.
        for request in host.suspend_all() {
            #[cfg(target_os = "macos")]
            if let host::Request::Notify(notifications) = request {
                notifications.raise();
            } else {
                reject("unexpected-cleanup-request");
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = request;
                reject("unexpected-cleanup-request");
            }
        }
        drop(host);
    }
}

fn setup(
    app: &mut tauri::App,
    observations: Option<Arc<lifecycle::Observations>>,
) -> NativeResult<()> {
    let checking = observations.is_some();
    #[cfg(target_os = "macos")]
    if checking {
        app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
    let count = if checking { 2 } else { 1 };
    let mut windows = Vec::with_capacity(count);
    for (index, label) in LABELS.into_iter().take(count).enumerate() {
        let window = tauri::WindowBuilder::new(app, label)
            .title("EASL managed native surface — unqualified input probe")
            .inner_size(800., 600.)
            .min_inner_size(320., 240.)
            .visible(false)
            .focused(false)
            .focusable(!checking)
            .build()
            .map_err(|_| "managed-window-create")?;
        if app.get_window(label).is_none() || app.get_webview(label).is_some() {
            return Err("managed-window-registration");
        }
        if let Some(observations) = &observations {
            observations.listen(&window, index);
        }
        windows.push(window);
    }
    let visible = windows[0].clone();
    app.wry_plugin(HookBuilder {
        windows,
        observations,
    });
    if FAILURE.with(Cell::get) || !HOST.with(|cell| cell.borrow().is_some()) {
        return Err("native-attachment-failed");
    }
    if !checking {
        visible.show().map_err(|_| "managed-window-show")?;
    }
    Ok(())
}

fn run_inner(
    context: tauri::Context<tauri::Wry>,
    observations: Option<Arc<lifecycle::Observations>>,
) -> Result<Option<serde_json::Value>, Box<dyn Error>> {
    if !context.config().app.windows.is_empty() {
        return Err("Native probe forbids configured WebView windows".into());
    }
    if HOST.with(|cell| cell.borrow().is_some()) {
        return Err("A native probe is already attached on this thread".into());
    }
    FAILURE.with(|failure| failure.set(false));
    let setup_observations = observations.clone();
    let built = tauri::Builder::default()
        .enable_macos_default_menu(false)
        .setup(move |app| {
            let result = setup(app, setup_observations);
            if result.is_err() {
                // Drop borrowed raw-handle users before failed setup drops the app.
                clear();
            }
            result.map_err(|stage| std::io::Error::other(stage).into())
        })
        .build(context);
    let app = match built {
        Ok(app) => app,
        Err(error) => {
            clear();
            return Err(error.into());
        }
    };
    let exit_code = app.run_return(move |handle, event| {
        if let Some(observations) = &observations {
            observations.run_event(handle, &event);
        }
    });
    let evidence = HOST.with(|cell| {
        let borrowed = cell.try_borrow().map_err(|_| "final-host-borrow")?;
        borrowed
            .as_ref()
            .ok_or("final-host-missing")?
            .check_result()
    });
    clear();
    if exit_code != 0 || FAILURE.with(Cell::get) {
        return Err("Native surface run recorded a failure".into());
    }
    evidence.map_err(Into::into)
}

/// Explicit foreground probe, never called by builds or component tests.
pub fn run(context: tauri::Context<tauri::Wry>) -> Result<(), Box<dyn Error>> {
    run_inner(context, None).map(|_| ())
}

/// Explicit native-runtime check. Creates two hidden windows, no webviews, no
/// clipboard access and no user document. This is not visual/IME/AX acceptance.
pub fn check_lifecycle(
    context: tauri::Context<tauri::Wry>,
) -> Result<serde_json::Value, Box<dyn Error>> {
    run_inner(context, Some(Arc::new(lifecycle::Observations::default())))?
        .ok_or_else(|| "Missing native lifecycle evidence".into())
}

pub fn build_info() -> serde_json::Value {
    serde_json::json!({
        "schema":"delysis.easl-tauri-probe.build.v1",
        "reviewed_api_versions":{"tauri":"2.11.5", "tauri_runtime":"2.11.3", "tauri_runtime_wry":"2.11.4", "tao":"0.35.3"},
        "implementation":"managed WindowBuilder + exact runtime-ID event hook + EASL layout/editing",
        "webviews_requested":0, "webview_dependencies_removed":false,
        "accessibility":"macOS AccessKit attachment over the real native text buffers; unqualified",
        "data":"ephemeral buffers; no project storage or model",
        "lifecycle_check":"explicit --check-native-lifecycle; hidden native windows, not a component mock",
        "missing":["native IME/preedit and candidate rectangle binding", "macOS accessibility native qualification", "non-macOS accessibility adapter", "Loom service integration", "native visual acceptance"],
        "qualified":false
    })
}
