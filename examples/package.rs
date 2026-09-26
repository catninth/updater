//! `cargo run --example package -- /path/to/consumer`
//! Requires an installed Tauri 2 CLI and signing variables in the environment.
use catninth_updater::installer::InstallerBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project = std::env::args_os()
        .nth(1)
        .ok_or("usage: package <consumer-project-directory>")?;
    InstallerBuilder::new(project).build()?;
    Ok(())
}
