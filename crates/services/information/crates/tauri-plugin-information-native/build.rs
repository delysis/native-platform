const COMMANDS: &[&str] = &[
    "information_status",
    "information_catalog_search",
    "information_installed",
    "information_resolve_install_plan",
    "information_query",
    "information_search",
    "information_read",
    "information_lookup",
    "information_register_external",
    "information_install",
    "information_mount_installation",
    "information_plan_removal",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tauri_plugin::Builder::new(COMMANDS).try_build()?;
    Ok(())
}
