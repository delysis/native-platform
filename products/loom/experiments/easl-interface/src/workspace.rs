//! Transient pane presentation over the shared, already-resolved settings.
use loom_config::{PaneConfig, PaneKind, PanePosition};
use loom_text_session::{
    worker::ProjectInfo,
    workspace::{EditorTarget, document_title, editor_target},
};
use loom_types::{DocumentId, ProjectId};

const POSITIONS: [PanePosition; 3] = [
    PanePosition::Main,
    PanePosition::Right,
    PanePosition::Bottom,
];

#[derive(Debug)]
pub struct Workspace {
    project: ProjectId,
    selected: [Option<String>; 3],
    hidden: [bool; 3],
    pub menu: Option<usize>,
    menu_cursor: usize,
    pub right_width: f32,
    pub bottom_height: f32,
    pub outline_width: f32,
}

impl Workspace {
    pub fn new(project: ProjectId) -> Self {
        Self {
            project,
            selected: [None, None, None],
            hidden: [false; 3],
            menu: None,
            menu_cursor: 0,
            right_width: 320.,
            bottom_height: 240.,
            outline_width: 220.,
        }
    }
    pub fn reset_project(&mut self, project: ProjectId) {
        if self.project != project {
            self.project = project;
            self.selected = [None, None, None];
            self.hidden = [false; 3];
            self.menu_cursor = 0;
        }
        // Like App.svelte, pane choices belong to the project while resized
        // dimensions remain window-local across project changes.
        self.menu = None;
    }
    pub fn choices(project: &ProjectInfo, position: usize) -> Vec<(&str, &PaneConfig)> {
        if !project.settings.enabled {
            return Vec::new();
        }
        project
            .settings
            .config
            .panes
            .iter()
            .filter(|(_, pane)| pane.visible && POSITIONS.get(position) == Some(&pane.position))
            .map(|(id, pane)| (id.as_str(), pane))
            .collect()
    }
    pub fn pane<'a>(
        &self,
        project: &'a ProjectInfo,
        position: usize,
    ) -> Option<(&'a str, &'a PaneConfig)> {
        let choices = Self::choices(project, position);
        choices
            .iter()
            .find(|(id, _)| self.selected.get(position).and_then(Option::as_deref) == Some(*id))
            .or_else(|| choices.first())
            .copied()
    }
    pub fn binding(&self, project: &ProjectInfo, position: usize, current: DocumentId) -> u16 {
        let Some((_, pane)) = self.pane(project, position) else {
            return 0;
        };
        if position == 0 {
            let settings_document = project
                .documents
                .iter()
                .find(|d| d.document_id == current)
                .is_some_and(|d| matches!(d.relative_path.as_str(), ".mine.toml" | ".loom.md"));
            if (pane.kind == PaneKind::Editor && pane.document.is_none()) || settings_document {
                return 0;
            }
        }
        match pane.kind {
            PaneKind::Editor => {
                match editor_target(pane.document.as_deref(), &project.documents, current) {
                    EditorTarget::Current => 1,
                    EditorTarget::Open(_) => 2,
                    EditorTarget::Unavailable => 3,
                }
            }
            PaneKind::Chat => 4,
            PaneKind::Terminal => 5,
            PaneKind::Browser => 6,
        }
    }
    pub fn visible(&self, position: usize) -> bool {
        self.hidden.get(position) == Some(&false)
    }
    pub fn resize(&mut self, id: crate::pane_divider::DividerId, size: f32) {
        use crate::pane_divider::DividerId;
        match id {
            DividerId::Outline => self.outline_width = size,
            DividerId::Right => self.right_width = size,
            DividerId::Bottom => self.bottom_height = size,
        }
    }
    pub fn view_id(&self, project: &ProjectInfo, position: usize) -> u16 {
        if position == 0 {
            return 1;
        }
        let Some((id, _)) = self.pane(project, position) else {
            return 0;
        };
        project
            .settings
            .config
            .panes
            .iter()
            .filter(|(_, pane)| pane.position != PanePosition::Main)
            .position(|(key, _)| key == id)
            .and_then(|index| u16::try_from(index + 3).ok())
            .unwrap_or(0)
    }
    pub fn toggle(&mut self, project: &ProjectInfo, position: usize) -> Result<(), String> {
        if position >= POSITIONS.len() || (position != 0 && self.pane(project, position).is_none())
        {
            return Err("Unknown pane".into());
        }
        // Visibility never owns an editor, a selection, an undo stack or a run.
        // The default manuscript is a main pane even without a configuration.
        self.hidden[position] = !self.hidden[position];
        self.menu = None;
        Ok(())
    }
    pub fn select(&mut self, project: &ProjectInfo, choice: usize) -> Result<(), String> {
        let position = self.menu.ok_or("Pane menu is closed")?;
        let choices = Self::choices(project, position);
        let (id, _) = choices.get(choice).ok_or("Unknown pane choice")?;
        self.selected[position] = Some((*id).into());
        self.menu = None;
        Ok(())
    }
    pub fn toggle_menu(&mut self, project: &ProjectInfo, position: usize) {
        let choices = Self::choices(project, position);
        self.menu = if self.menu != Some(position) && choices.len() > 1 {
            self.menu_cursor = choices
                .iter()
                .position(|(id, _)| self.selected[position].as_deref() == Some(*id))
                .unwrap_or(0);
            Some(position)
        } else {
            None
        };
    }
    pub fn menu_cursor(&self) -> usize {
        self.menu_cursor
    }
    /// All keys are consumed while a selector is open, including ordinary text.
    pub fn menu_key(&mut self, project: &ProjectInfo, key: u32) -> Result<(), String> {
        let Some(position) = self.menu else {
            return Ok(());
        };
        let count = Self::choices(project, position).len();
        if count == 0 {
            self.menu = None;
            return Ok(());
        }
        self.menu_cursor = self.menu_cursor.min(count - 1);
        match key {
            2 => self.select(project, self.menu_cursor)?,
            7 => self.menu_cursor = self.menu_cursor.saturating_sub(1),
            8 => self.menu_cursor = (self.menu_cursor + 1).min(count - 1),
            9 => self.menu_cursor = 0,
            10 => self.menu_cursor = count - 1,
            13 | 14 => self.menu = None,
            _ => {}
        }
        Ok(())
    }
    pub fn write_inputs(
        &self,
        project: &ProjectInfo,
        current: DocumentId,
        enabled: bool,
        input: &mut [f32; crate::interface::INPUT_COUNT],
    ) {
        for position in 0..3 {
            input[26 + position] = f32::from(self.binding(project, position, current));
            input[33 + position] = f32::from(
                u16::try_from(Self::choices(project, position).len())
                    .expect("bounded configured panes"),
            );
        }
        input[29] = f32::from(self.visible(1));
        input[30] = f32::from(self.visible(2));
        input[31] = self.menu.map_or(0., |position| {
            f32::from(u16::try_from(position + 1).expect("pane slot"))
        });
        input[32] = self.menu.map_or(0., |position| input[33 + position]);
        input[36] = self.right_width;
        input[37] = self.bottom_height;
        input[38] = f32::from(enabled);
        input[39] = f32::from(u16::try_from(self.menu_cursor).expect("bounded pane choices"));
        input[40] = f32::from(self.view_id(project, 1));
        input[41] = f32::from(self.view_id(project, 2));
        input[42] = self.outline_width;
        input[50] = f32::from(!self.visible(0));
    }
    pub fn open_target(
        &self,
        project: &ProjectInfo,
        position: usize,
        current: DocumentId,
    ) -> Option<DocumentId> {
        let (_, pane) = self.pane(project, position)?;
        match editor_target(pane.document.as_deref(), &project.documents, current) {
            EditorTarget::Open(id) => Some(id),
            _ => None,
        }
    }
    pub fn text(&self, project: &ProjectInfo, current: DocumentId, slot: u32) -> String {
        if (240..248).contains(&slot) {
            return self
                .menu
                .and_then(|position| {
                    Self::choices(project, position)
                        .get((slot - 240) as usize)
                        .map(|(id, pane)| pane.title.as_deref().unwrap_or(id).to_owned())
                })
                .unwrap_or_default();
        }
        let position = (slot % 10) as usize;
        let Some((id, pane)) = self.pane(project, position) else {
            return if slot == 210 {
                if self.visible(0) {
                    "Collapse main pane"
                } else {
                    "Show main pane"
                }
                .into()
            } else {
                String::new()
            };
        };
        let title = pane.title.as_deref().unwrap_or(id);
        match slot / 10 {
            20 => title.into(),
            21 => format!(
                "{} {title}",
                if self.visible(position) {
                    "Collapse"
                } else {
                    "Show"
                }
            ),
            22 => self
                .open_target(project, position, current)
                .and_then(|target| project.documents.iter().find(|d| d.document_id == target))
                .map_or_else(
                    || "Document unavailable".into(),
                    |document| format!("Open {}", document_title(document)),
                ),
            23 => match pane.kind {
                PaneKind::Editor => "Document unavailable".into(),
                PaneKind::Chat => "Chat is not yet connected in this native view.".into(),
                PaneKind::Terminal => {
                    "The terminal is not yet connected in this native view.".into()
                }
                PaneKind::Browser => {
                    "Page preview is not yet connected in this native view.".into()
                }
            },
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::Documents,
        interface::{Draw, Interface, SOURCE},
    };

    const CONFIG: &str = r#"
[workspace.panes.reader]
kind = "editor"
position = "right"
visible = true
title = "Reader"
document = "@document"
[workspace.panes.notes]
kind = "editor"
position = "right"
visible = true
title = "Notes"
document = '@"Reading notes.md"'
[workspace.panes.proof]
kind = "editor"
position = "bottom"
visible = true
title = "Proof"
document = "@document"
"#;

    fn fixture() -> (tempfile::TempDir, Documents) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".mine.toml"), CONFIG).unwrap();
        std::fs::write(root.path().join("Reading notes.md"), "Reference text.\n").unwrap();
        let docs = Documents::open(root.path()).unwrap();
        (root, docs)
    }

    fn scene(
        ui: &mut Interface,
        docs: &Documents,
        focus: u16,
        event: u16,
        key: u16,
    ) -> crate::interface::Scene {
        let mut input = [0.; crate::interface::INPUT_COUNT];
        input[0..2].copy_from_slice(&[1200., 800.]);
        input[2] = f32::from(event);
        input[5] = f32::from(key);
        input[10] = f32::from(focus);
        docs.workspace
            .write_inputs(&docs.project, docs.document_id(), true, &mut input);
        ui.step(input).unwrap()
    }

    #[test]
    fn configured_references_and_transient_choices_survive_hide_and_document_change() {
        let (_root, mut docs) = fixture();
        let current = docs.document_id();
        assert_eq!(docs.workspace.text(&docs.project, current, 201), "Notes");
        let notes_view = docs.workspace.view_id(&docs.project, 1);
        let notes = docs
            .project
            .documents
            .iter()
            .find(|d| d.relative_path == "Reading notes.md")
            .unwrap()
            .document_id;
        let other = DocumentId::new();
        assert_eq!(docs.workspace.binding(&docs.project, 1, other), 2);
        assert_eq!(
            docs.workspace.open_target(&docs.project, 1, other),
            Some(notes)
        );
        assert_eq!(docs.workspace.binding(&docs.project, 1, notes), 1);
        docs.workspace.toggle_menu(&docs.project, 1);
        docs.workspace.menu_key(&docs.project, 8).unwrap();
        docs.workspace.menu_key(&docs.project, 2).unwrap();
        assert_eq!(docs.workspace.text(&docs.project, current, 201), "Reader");
        assert_ne!(notes_view, docs.workspace.view_id(&docs.project, 1));
        assert_ne!(
            docs.workspace.view_id(&docs.project, 1),
            docs.workspace.view_id(&docs.project, 2)
        );
        for _ in 0..2 {
            docs.workspace.toggle(&docs.project, 1).unwrap();
        }
        docs.workspace.reset_project(docs.project.project_id);
        assert_eq!(docs.workspace.text(&docs.project, current, 201), "Reader");
        docs.workspace
            .resize(crate::pane_divider::DividerId::Right, 401.);
        docs.workspace.reset_project(ProjectId::new());
        assert_eq!(docs.workspace.right_width.to_bits(), 401_f32.to_bits());
        assert_eq!(docs.workspace.text(&docs.project, current, 201), "Notes");
        assert!(docs.workspace.select(&docs.project, 0).is_err());
        assert!(docs.workspace.toggle(&docs.project, 3).is_err());
    }

    #[test]
    fn current_configuration_drives_controls_editors_and_menu_input_routing() {
        let (_root, mut docs) = fixture();
        docs.workspace.toggle_menu(&docs.project, 1);
        docs.workspace.select(&docs.project, 1).unwrap();
        let right = docs.workspace.view_id(&docs.project, 1);
        let bottom = docs.workspace.view_id(&docs.project, 2);
        let mut ui = Interface::compile(SOURCE).unwrap();
        let shown = scene(&mut ui, &docs, right, 0, 0);
        assert_eq!(
            shown
                .controls
                .iter()
                .filter(|c| matches!(c.label_slot, Some(211 | 212)))
                .count(),
            2
        );
        let editors: Vec<_> = shown
            .draws
            .iter()
            .filter_map(|draw| match draw {
                Draw::Editor(rect, style, _) => Some((rect.0, style[2])),
                _ => None,
            })
            .collect();
        assert_eq!(editors.len(), 3);
        assert!(editors.iter().all(|(r, _)| r[0] >= 0.
            && r[1] >= 30.
            && r[0] + r[2] <= 1200.
            && r[1] + r[3] <= 800.));
        assert!(
            editors
                .iter()
                .any(|(_, id)| crate::interface::id(*id).ok() == Some(u32::from(bottom)))
        );
        docs.workspace.toggle(&docs.project, 1).unwrap();
        let hidden = scene(&mut ui, &docs, right, 2, 1);
        assert_eq!(crate::interface::id(hidden.state[2]).unwrap(), 1);
        assert_eq!(hidden.actions, [(1, 1)]);
        docs.workspace.toggle(&docs.project, 1).unwrap();
        docs.workspace.toggle_menu(&docs.project, 1);
        for key in [1, 2, 7, 8, 13, 14, 21] {
            assert_eq!(
                scene(&mut ui, &docs, right, 2, key).actions,
                [(21, u32::from(key))]
            );
        }
        let open = scene(&mut ui, &docs, right, 0, 0);
        assert_eq!(
            open.controls
                .iter()
                .filter(|c| c.label_slot.is_some_and(|slot| slot >= 240))
                .count(),
            2
        );
        assert!(
            open.controls
                .iter()
                .find(|c| c.label == "Add")
                .is_some_and(|c| !c.enabled)
        );
        docs.workspace.menu_key(&docs.project, 14).unwrap();
        assert!(docs.workspace.menu.is_none());
    }

    #[test]
    fn selector_dismissal_retains_control_focus_and_tab_uses_the_uncovered_scene() {
        use crate::focus::{Focus, Target, adjacent};
        let (_root, mut docs) = fixture();
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut focus = Focus::default();
        // Enter, Escape, Tab and a pointer selection all close the native select.
        for key in [2, 14, 13, 0] {
            let before = docs.workspace.menu;
            docs.workspace.toggle_menu(&docs.project, 1);
            focus.menu_changed(before, docs.workspace.menu);
            assert_eq!(focus.control(), Some(201));
            let menu = scene(&mut ui, &docs, 1, 0, 0);
            assert!(menu.controls.iter().any(|control| control.key == 240));
            // Pointer/AX activation may identify the option itself; that node
            // disappears after selection, while the parent selector survives.
            if key == 0 {
                focus.chrome = Some(Target::Control(241));
            }
            let before = docs.workspace.menu;
            if key == 0 {
                docs.workspace.select(&docs.project, 1).unwrap();
            } else {
                docs.workspace.menu_key(&docs.project, key).unwrap();
            }
            focus.menu_changed(before, docs.workspace.menu);
            let uncovered = scene(&mut ui, &docs, 1, 0, 0);
            assert!(!focus.reconcile(&uncovered));
            assert_eq!(focus.control(), Some(201));
            assert!(!uncovered.controls.iter().any(|control| control.key == 240));
            let next = adjacent(&uncovered, Target::Control(201), false).unwrap();
            assert_eq!(
                next,
                if docs.workspace.binding(&docs.project, 1, docs.document_id()) == 1 {
                    Target::Editor(u32::from(docs.workspace.view_id(&docs.project, 1)))
                } else {
                    Target::Control(221)
                }
            );
            assert_eq!(
                adjacent(&uncovered, Target::Control(201), true),
                Some(Target::Divider(crate::pane_divider::DividerId::Right))
            );
            let mut input = [0.; crate::interface::INPUT_COUNT];
            input[..2].copy_from_slice(&[1200., 800.]);
            input[2] = 2.;
            input[5] = 1.;
            input[10] = 1.;
            input[44] = 201.;
            docs.workspace
                .write_inputs(&docs.project, docs.document_id(), true, &mut input);
            assert!(ui.step(input).unwrap().actions.is_empty());
        }
    }
}
