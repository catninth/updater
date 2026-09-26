use catninth_updater::{Config, Repository, Updater};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repository = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "catninth/clipcat".into());
    let current = std::env::args().nth(2).unwrap_or_else(|| "0.1.0".into());
    let updater = Updater::builder(Config::new(Repository::parse(&repository)?, &current, 360)?)
        .on_event(|event| println!("{event:?}"))
        .build()?;
    updater.fetch_version().await?;
    println!("Installed: {}", updater.get_installed_version());
    if let Some(version) = updater.get_latest_version() {
        println!("Latest: {version}");
        println!(
            "{}",
            updater
                .get_patch_notes(&version.to_string())
                .await?
                .unwrap_or_default()
        );
    }
    Ok(())
}
