#![forbid(unsafe_code)]
//! Experimental EASL-authored native Loom interface.
mod accessibility;
mod actions;
mod chrome;
mod chrome_state;
#[cfg(target_os = "macos")]
mod companion;
#[cfg(test)]
mod current_ui_tests;
mod document;
mod document_model;
mod focus;
mod formatting;
#[cfg(test)]
mod frame_tests;
mod icon;
mod input_field;
mod input_geometry;
mod interface;
mod keyboard;
mod native_events;
mod pane_divider;
mod renderer;
mod text_field;
mod theme;
mod workspace;

use actions::Action;
use document::Documents;
use easl_native_text::EditCommand;
use interface::{Draw, Interface, Scene};
use loom_config::WorkspaceThemeMode as Appearance;
use renderer::Renderer;
use std::{num::NonZeroU32, path::PathBuf, sync::Arc, time::Instant};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, Ime, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Theme, Window, WindowId},
};

enum NativeEvent {
    Accessibility(accesskit_winit::Event),
    StorageAvailable,
}
impl From<accesskit_winit::Event> for NativeEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        Self::Accessibility(event)
    }
}

struct Native {
    ime_allowed: bool,
    access: accesskit_winit::Adapter,
    #[cfg(target_os = "macos")]
    companion: Option<companion::Companion>,
    window: Arc<Window>,
    _context: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
}
#[derive(Default)]
struct FrameState {
    pointer_pending: bool,
    dirty: bool,
}
impl FrameState {
    fn queue_pointer(&mut self) -> bool {
        !std::mem::replace(&mut self.pointer_pending, true)
    }
}
struct PreferenceView {
    appearance_preview: Option<Appearance>,
    system_dark: bool,
}
struct App {
    proxy: winit::event_loop::EventLoopProxy<NativeEvent>,
    next_access_id: u64,
    webview_enabled: bool,
    native: Option<Native>,
    startup_error: Option<String>,
    preferences: PreferenceView,
    renderer: Renderer,
    docs: Documents,
    ui: Interface,
    scene: Scene,
    ui_path: PathBuf,
    state: [f32; 4],
    pointer: [f32; 2],
    modifiers: ModifiersState,
    text: String,
    key: u32,
    repeat_guard: chrome_state::RepeatGuard<winit::keyboard::PhysicalKey>,
    scroll: f32,
    dragging: bool,
    divider: pane_divider::Interaction,
    focus: focus::Focus,
    frame: FrameState,
    composing_field: Option<u32>,
    last_click: Option<(Instant, [f32; 2])>,
    clicks: u8,
    format_palette: Option<formatting::Palette>,
    add_menu: chrome_state::AddMenu,
    zero_advance: f32,
}
impl App {
    fn input(&self, event: u32) -> [f32; crate::interface::INPUT_COUNT] {
        let mut input = [0.; crate::interface::INPUT_COUNT];
        let size = self.native.as_ref().map(|n| {
            n.window
                .inner_size()
                .to_logical::<f32>(n.window.scale_factor())
        });
        input[0] = size.map_or(1280., |s| s.width);
        input[1] = size.map_or(820., |s| s.height);
        input[2] = interface::small_number(event);
        input[3] = self.pointer[0];
        input[4] = self.pointer[1];
        input[5] = interface::small_number(self.key);
        input[6] = f32::from(self.command());
        input[7] = f32::from(self.modifiers.shift_key());
        input[8..12].copy_from_slice(&self.state);
        input[12] = f32::from(
            u16::try_from(self.docs.project.entries().len()).expect("bounded manuscript list"),
        );
        input[13] =
            f32::from(u16::try_from(self.docs.selected).expect("bounded manuscript selection"));
        input[14] = f32::from(self.docs.dirty());
        input[15] = f32::from(self.webview_enabled);
        input[16] = f32::from(
            self.docs
                .source_mode(interface::id(self.state[2]).unwrap_or(1)),
        );
        input[17] = f32::from(self.docs.context.source_mode());
        input[18] = f32::from(self.docs.manuscript.verse());
        let appearance = self
            .preferences
            .appearance_preview
            .unwrap_or(self.docs.project.settings.config.theme.mode);
        input[19] = f32::from(match appearance {
            Appearance::System => self.preferences.system_dark,
            Appearance::Light => false,
            Appearance::Dark => true,
        });
        input[20] = match appearance {
            Appearance::System => 0.,
            Appearance::Light => 1.,
            Appearance::Dark => 2.,
        };
        input[21] = self.zero_advance;
        input[22] = f32::from(self.format_palette.is_some());
        input[24] = self
            .format_palette
            .as_ref()
            .map_or(0., |palette| f32::from(palette.active_mask()));
        input[46] = f32::from(
            self.format_palette
                .as_ref()
                .is_some_and(formatting::Palette::can_link),
        );
        input[47] = f32::from(
            self.format_palette
                .as_ref()
                .is_some_and(formatting::Palette::can_unlink),
        );
        input[23] = f32::from(
            self.native
                .as_ref()
                .is_some_and(|n| n.window.fullscreen().is_some()),
        );
        input[25] = f32::from(self.docs.has_auxiliary());
        self.docs.workspace.write_inputs(
            &self.docs.project,
            self.docs.document_id(),
            self.docs.can_switch_views(),
            &mut input,
        );
        input[43] = self.focus.divider().map_or(0., |id| f32::from(id as u16));
        input[44] = self.focus.control().map_or(0., interface::small_number);
        input[45] = f32::from(self.focus.visible);
        input[48] = f32::from(self.add_menu.is_open());
        input[49] = f32::from(self.focus.chrome == Some(focus::Target::Window));
        // 51 is Ghost until an actual native assistance service supplies the
        // mode. A view projection does not grant model authority.
        input[52] = f32::from(self.docs.transitioning());
        input[53] = f32::from(!self.add_available());
        let theme = &self.docs.project.settings.config.theme;
        let (mask, colors) = theme::project([
            theme.canvas.as_deref(),
            theme.text.as_deref(),
            theme.accent.as_deref(),
        ]);
        input[54] = f32::from(mask);
        input[55..64].copy_from_slice(&colors);
        input
    }
    fn command(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.super_key()
        } else {
            self.modifiers.control_key() && !self.modifiers.alt_key()
        }
    }
    fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        eprintln!("{message}");
        self.docs.status = message.chars().take(220).collect();
    }
    fn update(&mut self, event: u32, loop_: &ActiveEventLoop) {
        self.frame.pointer_pending = false;
        self.refresh_scene(event, loop_);
        self.refresh_title();
        self.frame.dirty = true;
        if let Some(native) = &self.native {
            native.window.request_redraw();
        }
    }
    fn step_scene(&mut self, event: u32) -> Result<Scene, String> {
        let input = self.input(event);
        let scene = self.ui.step(input)?;
        actions::admit(event, &scene.actions)?;
        Ok(scene)
    }
    fn refresh_scene(&mut self, event: u32, loop_: &ActiveEventLoop) -> bool {
        if self.add_menu.is_open() && !self.add_available() {
            self.dismiss_add();
        }
        self.reconcile_formatting();
        self.move_divider();
        let event = if self.divider.dragging() && event == 5 {
            0
        } else {
            event
        };
        let previous = self.scene.clone();
        let mut changed;
        match self.step_scene(event) {
            Ok(mut scene) => {
                self.state = scene.state;
                // The full batch was validated by step_scene before installing
                // either presentation state or an externally visible command.
                let actions = std::mem::take(&mut scene.actions);
                self.scene = scene;
                changed = !actions.is_empty();
                for (kind, argument) in actions {
                    let action = Action::from_wire(kind, argument)
                        .expect("step_scene admitted the complete batch");
                    if let Err(error) = self.perform(action, loop_) {
                        self.error(error);
                    }
                }
                changed |= self.reconcile_formatting();
                // Reevaluate only when a host action may have changed the view's
                // inputs. Hover/layout events already produced their final scene.
                if changed {
                    match self.step_scene(0) {
                        Ok(scene) => {
                            self.state = scene.state;
                            self.scene = scene;
                        }
                        Err(e) => self.error(e),
                    }
                }
            }
            Err(e) => {
                self.error(e);
                changed = true;
            }
        }
        if self.docs.workspace.menu.is_none()
            && !self.add_menu.is_open()
            && self.focus.reconcile(&self.scene)
        {
            self.divider.release();
            match self.step_scene(0) {
                Ok(scene) => {
                    self.state = scene.state;
                    self.scene = scene;
                }
                Err(error) => self.error(error),
            }
        }
        for control in &mut self.scene.controls {
            if let Some(slot) = control.label_slot {
                control.label = self.docs.control_text(slot);
            }
        }
        changed |= previous != self.scene;
        self.update_pointer_cursor();
        changed
    }
    fn refresh_title(&self) {
        if let Some(native) = &self.native {
            native.window.set_title(&self.docs.title());
        }
    }
    fn transition(&mut self, destination: document::Destination) -> Result<(), String> {
        if self.docs.transitioning() {
            return Ok(());
        }
        let disposition = if self.docs.dirty() {
            let Some(native) = &self.native else {
                return Ok(());
            };
            match rfd::MessageDialog::new()
                .set_parent(native.window.as_ref())
                .set_title("Save your writing?")
                .set_description("Save changes to the manuscript and context before continuing?")
                .set_buttons(rfd::MessageButtons::YesNoCancel)
                .show()
            {
                rfd::MessageDialogResult::Yes => document::Disposition::Save,
                rfd::MessageDialogResult::No => document::Disposition::Discard,
                _ => return Ok(()),
            }
        } else {
            document::Disposition::Save
        };
        self.docs
            .begin_transition(destination, disposition, &mut self.renderer.text)?;
        self.dismiss_add();
        self.docs.workspace.menu = None;
        self.close_formatting();
        self.clear_chrome_focus();
        Ok(())
    }
    fn storage_tick(&mut self, loop_: &ActiveEventLoop) {
        match self.docs.tick(&mut self.renderer.text, Instant::now()) {
            Ok(true) => self.update(0, loop_),
            Ok(false) => {}
            Err(error) => {
                self.error(error);
                self.update(0, loop_);
            }
        }
        if self.docs.closed() {
            loop_.exit();
            return;
        }
        loop_.set_control_flow(self.docs.deadline().map_or(
            winit::event_loop::ControlFlow::Wait,
            winit::event_loop::ControlFlow::WaitUntil,
        ));
    }
    fn pointer_edit(&mut self, action: u32, arg: u32) -> Result<(), String> {
        if action == 2 {
            self.clear_chrome_focus();
        }
        let rect = self
            .scene
            .draws
            .iter()
            .find_map(|d| match d {
                Draw::Editor(rect, style, _) if interface::id(style[2]).ok() == Some(arg) => {
                    Some(*rect)
                }
                _ => None,
            })
            .ok_or("Text field is not visible")?;
        let field = self.docs.field(arg, &mut self.renderer.text)?;
        let x = self.pointer[0] - rect.0[0];
        let y = self.pointer[1] - rect.0[1] + field.editor.scroll;
        if action == 3 {
            field.editor.scroll =
                (field.editor.scroll + self.scroll * input_geometry::SCROLL_LINE_PIXELS).max(0.);
            field.editor.reveal_caret = false;
        } else {
            let edit = if action == 10 {
                EditCommand::Drag(x, y)
            } else {
                EditCommand::Click(x, y, self.clicks, self.modifiers.shift_key())
            };
            field.command(&mut self.renderer.text, edit)?;
        }
        if action == 2 {
            self.dragging = true;
        }
        Ok(())
    }
    fn perform(&mut self, action: Action, _loop: &ActiveEventLoop) -> Result<(), String> {
        if self.docs.transitioning()
            && !matches!(
                action,
                Action::Scroll(_) | Action::Reload | Action::Appearance(_) | Action::DragWindow
            )
        {
            return Ok(());
        }
        if self.add_menu.is_open()
            && !matches!(
                action,
                Action::AddMenu(_) | Action::AddNew | Action::Close | Action::Reload
            )
        {
            return Err("The Add menu owns input until it closes".into());
        }
        let (kind, arg) = action.wire();
        match action {
            Action::Edit(0) => {}
            Action::Edit(_) if self.focus.chrome.is_none() => self.edit(arg)?,
            Action::Edit(_) => {}
            Action::Click(_) | Action::Scroll(_) | Action::Drag(_) => {
                self.pointer_edit(kind, arg)?;
            }
            Action::Save => self.docs.save(&mut self.renderer.text)?,
            Action::OpenProject => self.open_project()?,
            Action::NewDocument => self.transition(document::Destination::NewDocument)?,
            Action::Auxiliary(_) => self.open_auxiliary(arg)?,
            Action::TogglePane(_)
            | Action::PaneMenu(_)
            | Action::SelectPane(_)
            | Action::OpenPane(_)
            | Action::PaneKey(_) => self.pane_action(kind, arg)?,
            Action::Resize(_) => self.begin_resize(arg)?,
            Action::ResizeKey(_) => self.resize_key(arg),
            Action::InputPointer(_) => self.pointer_input(arg)?,
            Action::AddMenu(_) | Action::AddNew => self.add_action(kind, arg)?,

            Action::Close => self.transition(document::Destination::Close)?,
            Action::SelectDocument(_) => {
                if arg as usize != self.docs.selected {
                    self.transition(document::Destination::Document(arg as usize))?;
                }
            }
            Action::Reload => self.reload_interface()?,
            Action::Source(_) => {
                self.close_formatting();
                let id = if arg == 0 {
                    interface::id(self.state[2])?
                } else {
                    arg
                };
                self.docs
                    .field(id, &mut self.renderer.text)?
                    .toggle_source(&mut self.renderer.text)?;
            }
            Action::Format(_) => {
                if self.composing_field == Some(formatting::DESTINATION) {
                    return Ok(());
                }
                if let Some(palette) = &mut self.format_palette {
                    let field = self.docs.field(palette.owner, &mut self.renderer.text)?;
                    palette.apply(field, &mut self.renderer.text, arg)?;
                    self.clear_chrome_focus();
                } else if self.focus.chrome.is_none() {
                    let id = interface::id(self.state[2])?;
                    self.docs
                        .field(id, &mut self.renderer.text)?
                        .format(&mut self.renderer.text, arg)?;
                }
            }
            Action::Appearance(_) => self.set_appearance(arg)?,
            Action::FormatMenu(_) => {
                if arg == 0 {
                    self.close_formatting();
                } else if self.focus.chrome.is_none() {
                    let owner = interface::id(self.state[2])?;
                    let field = self.docs.field(owner, &mut self.renderer.text)?;
                    self.format_palette = Some(formatting::Palette::open(owner, field)?);
                    self.focus_target(focus::Target::Control(300), true);
                }
            }
            Action::DragWindow => {
                if let Some(native) = &self.native {
                    native.window.drag_window().map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }
    fn open_project(&mut self) -> Result<(), String> {
        let Some(native) = &self.native else {
            return Ok(());
        };
        let chosen = rfd::FileDialog::new()
            .set_parent(native.window.as_ref())
            .set_title("Open a Loom project folder")
            .set_directory(self.docs.root())
            .pick_folder();
        if let Some(path) = chosen
            && path != self.docs.root()
        {
            self.transition(document::Destination::Project(path))?;
        }
        Ok(())
    }
    fn reload_interface(&mut self) -> Result<(), String> {
        let source = read_source(&self.ui_path)?;
        let mut candidate = Interface::compile(&source)?;
        let scene = candidate.step(self.input(0))?;
        actions::admit(0, &scene.actions)?;
        self.ui = candidate;
        self.docs.status = "Reloaded EASL interface · writing preserved".into();
        Ok(())
    }
    fn open_auxiliary(&mut self, index: u32) -> Result<(), String> {
        let document = self
            .docs
            .project
            .entries()
            .get(index as usize)
            .ok_or("Unknown pane document")?
            .document_id;
        self.docs.open_auxiliary(document)
    }
    fn pane_action(&mut self, action: u32, argument: u32) -> Result<(), String> {
        if !self.docs.can_switch_views() {
            return Ok(());
        }
        let position = argument as usize;
        let menu_before = self.docs.workspace.menu;
        let result = match action {
            17 => self.docs.workspace.toggle(&self.docs.project, position),
            18 => {
                self.docs
                    .workspace
                    .toggle_menu(&self.docs.project, position);
                Ok(())
            }
            21 => self.docs.workspace.menu_key(&self.docs.project, argument),
            19 => self.docs.workspace.select(&self.docs.project, position),
            20 => {
                let target = self
                    .docs
                    .workspace
                    .open_target(&self.docs.project, position, self.docs.document_id())
                    .ok_or("Pane document is unavailable")?;
                let index = self
                    .docs
                    .project
                    .entries()
                    .iter()
                    .position(|document| document.document_id == target)
                    .ok_or("Pane document is unavailable")?;
                self.transition(document::Destination::Document(index))
            }
            _ => Err("Unknown pane action".into()),
        };
        if matches!(action, 18 | 19 | 21) {
            self.focus
                .menu_changed(menu_before, self.docs.workspace.menu);
        }
        result
    }
    fn set_appearance(&mut self, arg: u32) -> Result<(), String> {
        let value = match arg {
            0 => Appearance::System,
            1 => Appearance::Light,
            2 => Appearance::Dark,
            _ => return Err("Invalid appearance preference".into()),
        };
        // Like the current webview, appearance commands are session overrides.
        // Persistent values come only from the shared project configuration.
        self.preferences.appearance_preview = Some(value);
        Ok(())
    }
    #[allow(
        clippy::too_many_lines,
        reason = "One command routing table keeps platform editing shortcuts inspectable"
    )]
    fn edit(&mut self, id: u32) -> Result<(), String> {
        if self.focus.chrome.is_some() {
            return Ok(());
        }
        if self.key == 14 && self.format_palette.is_some() {
            self.close_formatting();
            return Ok(());
        }
        let command = self.command();
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        let key = self.key;
        let field = self.docs.field(id, &mut self.renderer.text)?;
        let system = &mut self.renderer.text;
        if !command && !alt && key == 13 && shift {
            if !field.indent(system, loom_markdown::ListIndent::Outdent)? {
                self.traverse_focus(true);
            }
            return Ok(());
        }
        let edit = crate::keyboard::EditKey {
            key,
            command,
            shift,
            alt,
            text: &self.text,
        }
        .resolve(field.editor.selected_text())?;
        if let Some(edit) = edit {
            field.command(system, edit)?;
            // An explicit input in this focused editor may start a fresh
            // selection capture. Background changes cannot renew an old lease.
            if let Some(palette) = &mut self.format_palette
                && palette.owner == id
                && palette.renew_after_input(field).is_err()
            {
                self.close_formatting();
            }
        }
        Ok(())
    }
    fn keyboard_input(&mut self, event: winit::event::KeyEvent, loop_: &ActiveEventLoop) {
        self.key = key_id(&event.logical_key);
        let activation = (self.key == 2 || event.text.as_deref() == Some(" "))
            && (self.add_menu.is_open()
                || self.focus.control().is_some_and(|key| {
                    self.scene
                        .controls
                        .iter()
                        .any(|control| control.key == key && !control.input)
                }));
        let one_shot = chrome_state::one_shot(
            self.key,
            self.command(),
            self.modifiers.shift_key(),
            activation,
        );
        if self
            .repeat_guard
            .suppress(event.physical_key, event.repeat, one_shot)
        {
            return;
        }
        self.focus.record(if self.command() {
            "command key"
        } else {
            "ordinary key"
        });
        // Keep recovery available even when a reloaded VM has faulted.
        if self.key == 15 {
            if let Err(error) = self.perform(Action::Reload, loop_) {
                self.error(error);
            }
            self.update(0, loop_);
            return;
        }
        self.text = event.text.map_or_else(String::new, |t| t.to_string());
        if !self.add_key(loop_) && !self.chrome_key(loop_) {
            self.update(2, loop_);
        }
    }
    fn ime(&mut self, event: Ime, loop_: &ActiveEventLoop) {
        if input_field::owns_ime(self.composing_field, self.focus.control()) {
            self.input_ime(event, loop_);
            return;
        }
        if self.add_menu.is_open()
            || self.docs.workspace.menu.is_some()
            || self.focus.chrome.is_some()
            || self.state[2] == 0.
        {
            return;
        }
        let id = self
            .composing_field
            .unwrap_or_else(|| interface::id(self.state[2]).unwrap_or(1));
        // A transition may finish an existing candidate, but must not admit new
        // typing after its final save has been dispatched.
        if self.docs.transitioning()
            && self
                .docs
                .field(id, &mut self.renderer.text)
                .is_ok_and(|field| !field.is_composing())
        {
            self.composing_field = None;
            return;
        }
        let edit = match event {
            Ime::Enabled => return,
            Ime::Preedit(text, range) if !text.is_empty() => {
                self.composing_field = Some(id);
                EditCommand::Preedit(text, range)
            }
            Ime::Disabled | Ime::Preedit(_, _) => {
                self.composing_field = None;
                EditCommand::CancelCompose
            }
            Ime::Commit(text) => {
                self.composing_field = None;
                EditCommand::Commit(text)
            }
        };
        let result = self
            .docs
            .field(id, &mut self.renderer.text)
            .and_then(|field| field.command(&mut self.renderer.text, edit));
        if let Err(error) = result {
            self.error(error);
        }
        self.update(0, loop_);
    }

    fn paint(&mut self) -> Result<(), String> {
        let Some(native) = &mut self.native else {
            return Ok(());
        };
        let size = native.window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(());
        };
        if u64::from(size.width) * u64::from(size.height) > 16 * 1024 * 1024 {
            return Err("Window exceeds the experimental raster size limit".into());
        }
        native
            .surface
            .resize(width, height)
            .map_err(|e| e.to_string())?;
        let mut pixels = native.surface.buffer_mut().map_err(|e| e.to_string())?;
        self.renderer.paint(
            &mut pixels,
            size.width as usize,
            size.height as usize,
            interface::logical_pixels(native.window.scale_factor()),
            &self.scene,
            &mut self.docs,
            self.format_palette
                .as_mut()
                .map(|p| (formatting::DESTINATION, &mut p.destination)),
        )?;
        if self.focus.control() == Some(formatting::DESTINATION) {
            if let Some(palette) = &self.format_palette
                && let Some(Draw::Input(view)) = self.scene.draws.iter().find(
                    |draw| matches!(draw, Draw::Input(view) if view.key == formatting::DESTINATION),
                )
            {
                let area = palette.destination.field.editor.inner().ime_cursor_area();
                native.window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(
                        f64::from(view.content.0[0] - palette.destination.scroll_x) + area.x0,
                        f64::from(view.content.0[1] - palette.destination.field.editor.scroll)
                            + area.y0,
                    ),
                    LogicalSize::new(area.width().max(1.), area.height().max(1.)),
                );
            }
        } else if let Ok(field) = self.docs.field(
            interface::id(self.state[2]).unwrap_or_default(),
            &mut self.renderer.text,
        ) {
            let area = field.editor.inner().ime_cursor_area();
            if let Some(rect) = self.scene.draws.iter().find_map(|d| match d {
                Draw::Editor(rect, style, _) if style[2].to_bits() == self.state[2].to_bits() => {
                    Some(rect)
                }
                _ => None,
            }) {
                native.window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(
                        f64::from(rect.0[0]) + area.x0,
                        f64::from(rect.0[1] - field.editor.scroll) + area.y0,
                    ),
                    LogicalSize::new((area.x1 - area.x0).max(1.), (area.y1 - area.y0).max(1.)),
                );
            }
        }
        pixels.present().map_err(|e| e.to_string())?;
        #[cfg(target_os = "macos")]
        if let Some(companion) = &mut native.companion {
            let rect = self.scene.draws.iter().find_map(|d| match d {
                Draw::WebView(rect) => Some(rect.0),
                _ => None,
            });
            companion.update(rect)?;
        }
        self.update_accessibility();
        Ok(())
    }
}
impl ApplicationHandler<NativeEvent> for App {
    fn about_to_wait(&mut self, loop_: &ActiveEventLoop) {
        self.storage_tick(loop_);
    }
    fn resumed(&mut self, loop_: &ActiveEventLoop) {
        if self.native.is_some() {
            return;
        }
        let result = (|| {
            let attributes = Window::default_attributes()
                .with_visible(false)
                .with_title("Loom")
                .with_inner_size(LogicalSize::new(1280., 820.))
                .with_min_inner_size(LogicalSize::new(640., 480.));
            #[cfg(target_os = "macos")]
            let attributes = {
                use winit::platform::macos::WindowAttributesExtMacOS;
                attributes
                    .with_titlebar_transparent(true)
                    .with_title_hidden(true)
                    .with_fullsize_content_view(true)
            };
            let window = Arc::new(loop_.create_window(attributes).map_err(|e| e.to_string())?);
            let access =
                accesskit_winit::Adapter::with_event_loop_proxy(loop_, &window, self.proxy.clone());
            window.set_ime_allowed(true);
            let context = softbuffer::Context::new(window.clone()).map_err(|e| e.to_string())?;
            let surface =
                softbuffer::Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;
            #[cfg(target_os = "macos")]
            let companion = if self.webview_enabled {
                Some(companion::Companion::new(&window)?)
            } else {
                None
            };
            self.preferences.system_dark = window.theme() == Some(Theme::Dark);
            window.set_visible(true);
            Ok::<_, String>(Native {
                ime_allowed: true,
                #[cfg(target_os = "macos")]
                companion,
                window,
                access,
                _context: context,
                surface,
            })
        })();
        match result {
            Ok(native) => {
                self.native = Some(native);
                self.update(0, loop_);
            }
            Err(e) => {
                self.startup_error = Some(e);
                loop_.exit();
            }
        }
    }
    fn user_event(&mut self, loop_: &ActiveEventLoop, event: NativeEvent) {
        match event {
            NativeEvent::Accessibility(event) => self.accessibility_event(loop_, event),
            NativeEvent::StorageAvailable => self.storage_tick(loop_),
        }
    }
    fn window_event(&mut self, loop_: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        if self
            .native
            .as_ref()
            .is_none_or(|native| native.window.id() != window_id)
        {
            return;
        }
        if let Some(native) = &mut self.native {
            native.access.process_event(&native.window, &event);
        }
        match event {
            WindowEvent::CloseRequested => self.update(4, loop_),
            WindowEvent::RedrawRequested => self.redraw(loop_),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.update(0, loop_);
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::Focused(false) => {
                self.dismiss_add();
                let before = self.docs.workspace.menu.take();
                self.focus.menu_changed(before, None);
                self.focus.record("window blurred");
                self.modifiers = ModifiersState::empty();
                self.dragging = false;
                self.divider.release();
                self.update(0, loop_);
            }
            WindowEvent::Focused(true) => {
                self.focus.record("window focused");
                self.update(0, loop_);
            }
            WindowEvent::ThemeChanged(theme) => {
                self.preferences.system_dark = theme == Theme::Dark;
                self.update(0, loop_);
            }
            WindowEvent::CursorMoved { position, .. } => self.cursor_moved(position),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => self.left_button(state, loop_),
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta, loop_),
            WindowEvent::Ime(event) => self.ime(event, loop_),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Released => {
                self.repeat_guard.release(&event.physical_key);
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.composing_field.is_none() =>
            {
                self.keyboard_input(event, loop_);
            }
            _ => {}
        }
    }
}
fn key_id(key: &Key) -> u32 {
    match key {
        Key::Named(named) => match named {
            NamedKey::Enter => 2,
            NamedKey::Backspace => 3,
            NamedKey::Delete => 4,
            NamedKey::ArrowLeft => 5,
            NamedKey::ArrowRight => 6,
            NamedKey::ArrowUp => 7,
            NamedKey::ArrowDown => 8,
            NamedKey::Home => 9,
            NamedKey::End => 10,
            NamedKey::PageUp => 11,
            NamedKey::PageDown => 12,
            NamedKey::Tab => 13,
            NamedKey::Escape => 14,
            NamedKey::F5 => 15,
            _ => 0,
        },
        Key::Character(text) => match text.to_lowercase().as_str() {
            "a" => 16,
            "c" => 17,
            "v" => 18,
            "x" => 19,
            "z" => 20,
            "s" => 21,
            "o" => 22,
            "n" => 23,
            "q" => 24,
            "b" => 25,
            "i" => 26,
            "m" => 28,
            "f" => 29,
            _ => 1,
        },
        _ => 0,
    }
}
fn read_source(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let mut source = String::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take((interface::MAX_SOURCE + 1) as u64)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() > interface::MAX_SOURCE {
        return Err("EASL source exceeds 64 KiB".into());
    }
    Ok(source)
}
fn local_data_root() -> Result<PathBuf, String> {
    let user_home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is missing; provide --project".to_owned())
    };
    let base = if cfg!(target_os = "macos") {
        user_home()?.join("Library/Application Support")
    } else if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or("LOCALAPPDATA is missing; provide --project")?
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .map_or_else(|| user_home().map(|p| p.join(".local/share")), Ok)?
    };
    Ok(base)
}
struct Options {
    project: PathBuf,
    ui_path: PathBuf,
    explicit_ui: bool,
    webview_enabled: bool,
}
fn options() -> Result<Options, String> {
    let mut project = std::env::var_os("LOOM_EASL_PROJECT").map(PathBuf::from);
    let source = std::env::var_os("LOOM_EASL_UI");
    let mut explicit_ui = source.is_some();
    let mut ui_path = source.map_or_else(|| PathBuf::from(interface::SOURCE_PATH), PathBuf::from);
    let mut webview_enabled = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--webview") => {
                if !cfg!(target_os = "macos") {
                    return Err(
                        "The companion webview experiment is currently available on macOS".into(),
                    );
                }
                webview_enabled = true;
            }
            Some("--project") => {
                project = Some(PathBuf::from(
                    args.next().ok_or("--project needs a folder")?,
                ));
            }
            Some("--ui") => {
                ui_path = args.next().ok_or("--ui needs an EASL file")?.into();
                explicit_ui = true;
            }
            _ => {
                return Err(
                    "Usage: loom-easl-interface [--project FOLDER] [--ui FILE] [--webview]".into(),
                );
            }
        }
    }
    let project = project.map_or_else(
        || local_data_root().map(|p| p.join("Loom EASL Experiment/Project")),
        Ok,
    )?;
    Ok(Options {
        project,
        ui_path,
        explicit_ui,
        webview_enabled,
    })
}
pub fn run() -> Result<(), String> {
    // Keep the entire host type-checked in review builds, but exit before any
    // options, project, event loop, dialog, webview or foreground window exists.
    if cfg!(feature = "review-only") {
        return Err("Review-only builds cannot launch the native application".into());
    }
    if std::env::args_os().any(|arg| arg == "--build-info") {
        println!(
            "{}",
            serde_json::json!({"package":"loom-easl-interface","source_commit":option_env!("EASL_BUILD_COMMIT").unwrap_or("unrecorded development build"),"interface_source":interface::SOURCE_PATH,"interface_reference":env!("LOOM_UI_REFERENCE"),"interface_reference_sha256":env!("LOOM_UI_REFERENCE_SHA256"),"reference_manifest":serde_json::from_str::<serde_json::Value>(include_str!(concat!(env!("OUT_DIR"), "/loom_ui_reference.json"))).expect("build-generated reference manifest"),"native_text":"Parley 0.11.1 EASL fork + Vello CPU 0.2.0","companion_webview":"optional Wry 0.55.1 on macOS"})
        );
        return Ok(());
    }
    let Options {
        project,
        ui_path,
        explicit_ui,
        webview_enabled,
    } = options()?;
    let source = if explicit_ui || ui_path.exists() {
        read_source(&ui_path)?
    } else {
        interface::SOURCE.into()
    };
    let ui = Interface::compile(&source)?;
    let mut renderer = Renderer::new();
    renderer
        .prepare_assets()
        .map_err(|error| error.to_string())?;
    let zero_advance = renderer.body_character_width().map_err(|e| e.to_string())?;
    let event_loop = EventLoop::<NativeEvent>::with_user_event()
        .build()
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&project).map_err(|error| error.to_string())?;
    let storage_proxy = event_loop.create_proxy();
    let docs = Documents::open_with_notify(&project, move || {
        let _ = storage_proxy.send_event(NativeEvent::StorageAvailable);
    })?;
    let mut app = App {
        proxy: event_loop.create_proxy(),
        next_access_id: 10000,
        native: None,
        startup_error: None,
        preferences: PreferenceView {
            appearance_preview: None,
            system_dark: false,
        },
        webview_enabled,
        renderer,
        docs,
        ui,
        scene: Scene::default(),
        ui_path,
        state: [0., 0., 1., 19.],
        pointer: [-1., -1.],
        modifiers: ModifiersState::empty(),
        text: String::new(),
        key: 0,
        repeat_guard: chrome_state::RepeatGuard::default(),
        scroll: 0.,
        dragging: false,
        divider: pane_divider::Interaction::default(),
        focus: focus::Focus::default(),
        frame: FrameState {
            pointer_pending: false,
            dirty: true,
        },
        composing_field: None,
        last_click: None,
        clicks: 0,
        format_palette: None,
        add_menu: chrome_state::AddMenu::default(),
        zero_advance,
    };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    app.startup_error.map_or(Ok(()), Err)
}

#[cfg(all(test, feature = "review-only"))]
mod review_build_tests {
    #[test]
    fn public_entrypoint_refuses_to_open_an_application() {
        assert_eq!(
            super::run().unwrap_err(),
            "Review-only builds cannot launch the native application"
        );
    }

    #[test]
    fn review_manifest_is_explicitly_unqualified_and_reports_expected_hash() {
        let value: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("OUT_DIR"),
            "/loom_ui_reference.json"
        )))
        .unwrap();
        assert_eq!(value["qualification"], "unqualified-review-only");
        assert_eq!(
            value["expected_app_sha256"],
            env!("LOOM_UI_EXPECTED_APP_SHA256")
        );
        assert_eq!(
            value["icons"].as_object().unwrap().len(),
            crate::icon::KEYS.len()
        );
    }
}
