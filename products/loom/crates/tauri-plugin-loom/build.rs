const COMMANDS: &[&str] = &[
    "project_choose_create",
    "project_choose_open",
    "project_close",
    "project_current",
    "project_recover",
    "document_open",
    "document_checkpoint",
    "document_draft_upsert",
    "document_draft_clear",
    "document_reconciliation_preview",
    "document_reconcile_apply",
    "model_list",
    "focus_mode_set",
    "application_close",
];

fn main() {
    if let Err(error) = tauri_plugin::Builder::new(COMMANDS).try_build() {
        panic!("failed to build Loom plugin metadata: {error}");
    }
}
