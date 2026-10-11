//! Tests execute the production EASL VM and shared document/view adapter.
//! They do not claim pixels, OS focus, a live model or signed-bundle acceptance.
use crate::{
    actions,
    chrome_state::ADD_LABELS,
    document::Documents,
    focus::{self, Focus, Target},
    interface::{Control, Draw, INPUT_COUNT, Interface, SOURCE, Scene},
};
use easl_native_text::{EditCommand, TextSystem};

fn input() -> [f32; INPUT_COUNT] {
    let mut input = [0.; INPUT_COUNT];
    input[..2].copy_from_slice(&[1280., 820.]);
    input[8..12].copy_from_slice(&[0., 0., 1., 19.]);
    input[12] = 2.;
    input[38] = 1.;
    input
}

fn control(scene: &Scene, key: u32) -> &Control {
    scene
        .controls
        .iter()
        .find(|control| control.key == key)
        .unwrap()
}

fn click(ui: &mut Interface, mut input: [f32; INPUT_COUNT], key: u32) -> Scene {
    let scene = ui.step(input).unwrap();
    let rect = control(&scene, key).rect.0;
    input[2] = 1.;
    input[3] = rect[0] + rect[2] * 0.5;
    input[4] = rect[1] + rect[3] * 0.5;
    let scene = ui.step(input).unwrap();
    actions::admit(1, &scene.actions).unwrap();
    scene
}

#[test]
fn add_is_a_menu_and_only_the_service_backed_choice_dispatches() {
    let mut ui = Interface::compile(SOURCE).unwrap();
    let mut input = input();
    assert_eq!(click(&mut ui, input, 2).actions, [(25, 1)]);
    input[48] = 1.;
    input[44] = 400.;
    let scene = ui.step(input).unwrap();
    for (offset, label) in ADD_LABELS.iter().enumerate() {
        let key = 400 + u32::try_from(offset).unwrap();
        let item = control(&scene, key);
        assert_eq!(&item.label, label);
        assert_eq!(item.enabled, offset == 0);
        let actions = click(&mut ui, input, key).actions;
        assert_eq!(actions, if offset == 0 { vec![(26, 0)] } else { vec![] });
        if offset != 0 {
            let hover = click(&mut ui, input, key);
            assert!(
                hover
                    .draws
                    .iter()
                    .any(|draw| matches!(draw, Draw::Slot(_, _, slot) if *slot == key + 512))
            );
        }
    }
    assert_eq!(focus::targets(&scene), [Target::Control(400)]);
}

#[test]
fn open_menu_consumes_editor_hits_and_keys_without_click_through() {
    let mut ui = Interface::compile(SOURCE).unwrap();
    let mut input = input();
    input[48] = 1.;
    input[44] = 400.;
    input[2] = 2.;
    for command in [0., 1.] {
        input[6] = command;
        for key in 0_u16..=29 {
            input[5] = f32::from(key);
            assert!(ui.step(input).unwrap().actions.is_empty());
        }
    }
    input[2] = 1.;
    input[3..5].copy_from_slice(&[600., 300.]);
    assert_eq!(ui.step(input).unwrap().actions, [(25, 0)]);
}

#[test]
fn current_titlebar_has_one_mode_button_and_three_distinct_pane_controls() {
    let mut ui = Interface::compile(SOURCE).unwrap();
    let mut input = input();
    input[27..31].fill(1.);
    input[33] = 1.;
    let scene = ui.step(input).unwrap();
    assert_eq!(control(&scene, 210).toggled, Some(true));
    assert_eq!(control(&scene, 211).toggled, Some(true));
    assert_eq!(control(&scene, 212).toggled, Some(true));
    assert!(control(&scene, 210).rect.0[0] < control(&scene, 211).rect.0[0]);
    assert!(control(&scene, 211).rect.0[0] < control(&scene, 212).rect.0[0]);
    assert_eq!(control(&scene, 4).label, "Ghost text");
    assert!(!control(&scene, 4).enabled);
    assert!(!scene.controls.iter().any(|c| matches!(c.key, 5 | 6)));
    input[51] = 1.;
    let loompad = ui.step(input).unwrap();
    assert_eq!(control(&loompad, 4).label, "Loompad");
    assert_ne!(scene.draws, loompad.draws);
    // The unconfigured main fallback follows the configured right/bottom slots.
    input[33] = 0.;
    let fallback = ui.step(input).unwrap();
    assert!(control(&fallback, 212).rect.0[0] < control(&fallback, 210).rect.0[0]);
}

#[test]
fn titlebar_controls_remain_bounded_in_narrow_light_dark_and_fullscreen_views() {
    let mut ui = Interface::compile(SOURCE).unwrap();
    for width in [320_u16, 620, 640, 1280] {
        for dark in [0., 1.] {
            for fullscreen in [0., 1.] {
                let mut input = input();
                input[0] = f32::from(width);
                input[19] = dark;
                input[23] = fullscreen;
                input[8] = 1.;
                input[26..31].fill(1.);
                input[33] = 1.;
                let scene = ui.step(input).unwrap();
                let mut rectangles: Vec<_> = scene
                    .controls
                    .iter()
                    .filter(|c| c.rect.0[1] < 30.)
                    .map(|c| c.rect.0)
                    .collect();
                rectangles.sort_by(|a, b| a[0].total_cmp(&b[0]));
                for draw in &scene.draws {
                    if let Draw::Editor(rect, _, _) = draw {
                        assert!(rect.0[0] >= 0. && rect.0[0] + rect.0[2] <= input[0]);
                    }
                }
                for rect in &rectangles {
                    assert!(rect[0] >= 0. && rect[0] + rect[2] <= input[0]);
                    assert_eq!(rect[2..], [24., 24.]);
                }
                for pair in rectangles.windows(2) {
                    assert!(pair[0][0] + pair[0][2] <= pair[1][0]);
                }
            }
        }
    }
}

#[test]
fn main_collapse_uses_remaining_panes_and_never_edits_a_hidden_field() {
    let mut ui = Interface::compile(SOURCE).unwrap();
    let mut input = input();
    input[50] = 1.;
    input[2] = 2.;
    input[5] = 1.;
    let empty = ui.step(input).unwrap();
    assert_eq!(empty.state[2].to_bits(), 0_f32.to_bits());
    assert!(empty.actions.is_empty());
    assert!(
        !empty
            .draws
            .iter()
            .any(|draw| matches!(draw, Draw::Editor(..)))
    );
    input[27] = 1.;
    input[29] = 1.;
    input[40] = 3.;
    let right = ui.step(input).unwrap();
    assert_eq!(right.state[2].to_bits(), 3_f32.to_bits());
    assert_eq!(right.actions, [(1, 3)]);
    assert!(right.dividers.is_empty());
    input[29] = 0.;
    input[28] = 1.;
    input[30] = 1.;
    input[41] = 4.;
    let bottom = ui.step(input).unwrap();
    assert_eq!(bottom.actions, [(1, 4)]);
    assert!(bottom.dividers.is_empty());
}

#[test]
fn disabled_and_missing_chrome_cannot_implicitly_restore_editor_authority() {
    let mut ui = Interface::compile(SOURCE).unwrap();
    let mut input = input();
    let mut focus = Focus::default();
    focus.chrome = Some(Target::Control(2));
    input[53] = 1.;
    let disabled = ui.step(input).unwrap();
    assert!(!control(&disabled, 2).enabled);
    assert!(!focus.reconcile(&disabled));
    input[44] = 2.;
    input[2] = 2.;
    input[5] = 1.;
    assert!(ui.step(input).unwrap().actions.is_empty());
    assert!(focus.reconcile(&Scene::default()));
    assert_eq!(focus.chrome, Some(Target::Window));
    input[44] = 0.;
    input[49] = 1.;
    assert!(ui.step(input).unwrap().actions.is_empty());
}

#[test]
fn configured_theme_black_is_not_confused_with_absence_or_geometry() {
    let mut ui = Interface::compile(SOURCE).unwrap();
    let mut input = input();
    let original = ui.step(input).unwrap();
    input[54] = 1.; // Explicit black canvas, not the default light canvas.
    let black = ui.step(input).unwrap();
    assert_ne!(original.draws[0], black.draws[0]);
    let bounds = |scene: &Scene| {
        scene
            .controls
            .iter()
            .map(|c| (c.key, c.rect))
            .collect::<Vec<_>>()
    };
    assert_eq!(bounds(&original), bounds(&black));
}

#[test]
fn hide_and_show_retains_shared_source_revision_selection_mode_and_undo() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".mine.toml"),
        "[workspace.panes.reader]\nkind='editor'\nposition='right'\nvisible=true\ndocument='@document'\n").unwrap();
    let mut docs = Documents::open(root.path()).unwrap();
    let mut system = TextSystem::new();
    assert!(
        !docs.manuscript.verse(),
        "This retention fixture requires the default prose document"
    );
    let original = docs.manuscript.text();
    docs.manuscript
        .command(&mut system, EditCommand::SelectAll)
        .unwrap();
    docs.manuscript
        .command(&mut system, EditCommand::Insert("alpha β".into()))
        .unwrap();
    let mut projected = input();
    docs.workspace
        .write_inputs(&docs.project, docs.document_id(), true, &mut projected);
    let view = crate::interface::id(projected[40]).unwrap();
    let field = docs.field(view, &mut system).unwrap();
    field.toggle_source(&mut system).unwrap();
    field.command(&mut system, EditCommand::SelectAll).unwrap();
    field.editor.scroll = 17.;
    field.editor.reveal_caret = false;
    let source = field.text();
    let revision = field.revision();
    let selection = field.editor.selected_text().map(str::to_owned);
    let mut ui = Interface::compile(SOURCE).unwrap();
    let mut input = input();
    input[10] = crate::interface::small_number(view);
    docs.workspace.toggle(&docs.project, 1).unwrap();
    docs.workspace
        .write_inputs(&docs.project, docs.document_id(), true, &mut input);
    let hidden = ui.step(input).unwrap();
    assert!(!hidden.draws.iter().any(|draw| matches!(draw, Draw::Editor(_, style, _) if crate::interface::id(style[2]).ok() == Some(view))));
    // Visiting another actual view exercises retained selection/history ownership.
    docs.field(1, &mut system).unwrap();
    docs.workspace.toggle(&docs.project, 1).unwrap();
    assert!(docs.source_mode(view));
    let field = docs.field(view, &mut system).unwrap();
    assert_eq!(field.text(), source);
    assert_eq!(field.revision(), revision);
    assert_eq!(field.editor.selected_text().map(str::to_owned), selection);
    assert_eq!(field.editor.scroll.to_bits(), 17_f32.to_bits());
    field.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(field.text(), original);
}
