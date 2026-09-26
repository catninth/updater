//! Compiled integration helper; use from a consumer's setup callback after
//! registering tauri_plugin_updater. This example does not launch a GUI.
use std::sync::Arc;

use catninth_updater::{
    Config, Event, InstallHooks, Release, Repository, Result, Updater, tauri::TauriInstaller,
};
use tauri::{AppHandle, Emitter, Runtime};

struct AppHooks<R: Runtime>(AppHandle<R>);
#[async_trait::async_trait]
impl<R: Runtime> InstallHooks for AppHooks<R> {
    async fn after_install(&self, _release: &Release) -> Result<()> {
        // Consumer: stop sidecars and call tauri_plugin_single_instance::destroy
        // before restart. Windows uses on_before_exit instead.
        self.0.restart();
    }
}

pub fn create_updater<R: Runtime>(app: AppHandle<R>, public_key: &str) -> Result<Updater> {
    let repository = Repository::parse("catninth/clipcat")?;
    let config = Config::new(
        repository.clone(),
        &app.package_info().version.to_string(),
        360,
    )?;
    let installer = TauriInstaller::new(app.clone(), repository, public_key)?
        .with_timeout(config.request_timeout())?
        .on_before_exit(|| {
            // Consumer: stop the capture engine, flush state, release instance lock.
        });
    let events = app.clone();
    Updater::builder(config)
        .installer(Arc::new(installer))
        .hooks(Arc::new(AppHooks(app)))
        .on_event(move |event| {
            let _ = events.emit("updater", event);
            if let Event::UpdateAvailable(release) = event {
                println!("Update available: {}", release.version);
            }
        })
        .build()
}

fn main() {}
