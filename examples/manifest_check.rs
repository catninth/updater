use catninth_updater::{Config, Repository, Updater, manifest::GitHubManifestSource};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repository = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "catninth/clipcat".into());
    let current = std::env::args().nth(2).unwrap_or_else(|| "0.1.0".into());
    let config = Config::new(Repository::parse(&repository)?, &current, 360)?;
    let source = GitHubManifestSource::new(config.repository().clone(), config.request_timeout())?;
    let updater = Updater::builder(config).source(Arc::new(source)).build()?;
    updater.fetch_version().await?;
    println!("Installed: {}", updater.get_installed_version());
    if let Some(release) = updater.state().latest {
        println!("Latest: {} ({})", release.version, release.tag);
        println!("{}", release.notes.unwrap_or_default());
        println!(
            "Tag notes: {}",
            updater
                .get_patch_notes(&release.tag)
                .await?
                .unwrap_or_default()
        );
    }
    Ok(())
}
