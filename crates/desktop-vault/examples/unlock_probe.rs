//! Opt-in OS credential acceptance. Writes only an explicitly supplied fixture
//! directory; never prints a credential or inspects any user project.
use std::error::Error;
use std::path::PathBuf;

use desktop_vault::ProjectVault;

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: unlock_probe /absolute/isolated/fixture")?;
    if !root.is_absolute() {
        return Err("fixture path must be absolute".into());
    }
    std::fs::create_dir_all(root.join(".loom"))?;
    let vault = match ProjectVault::open(&root)? {
        Some(vault) => vault,
        None => ProjectVault::initialize(&root)?,
    };
    let path = root.join("probe.enc");
    if path.exists() {
        if vault.open_bytes("acceptance/probe", &std::fs::read(path)?, 64)?
            != b"Mine unlock acceptance"
        {
            return Err("probe content differs".into());
        }
    } else {
        std::fs::write(
            path,
            vault.seal("acceptance/probe", b"Mine unlock acceptance")?,
        )?;
    }
    for _ in 0..16 {
        ProjectVault::open(&root)?.ok_or("vault disappeared")?;
    }
    println!(
        "verified {}",
        option_env!("MINE_VAULT_PROBE_REVISION").unwrap_or("local")
    );
    Ok(())
}
