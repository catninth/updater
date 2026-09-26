//! Optional native adapter. Register `tauri_plugin_updater::Builder` on the app first.
//! Signature verification and OS-specific installation are delegated to Tauri 2.

use std::{sync::Arc, time::Duration};

use ::tauri::{AppHandle, Runtime};
use async_trait::async_trait;
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{
    DownloadCallback, Error, InstallBackend, PreparedUpdate, Release, Repository, Result,
    config::parse_version, installer::decode_public_key,
};

pub struct TauriInstaller<R: Runtime> {
    app: AppHandle<R>,
    repository: Repository,
    public_key: String,
    timeout: Duration,
    before_exit: Arc<dyn Fn() + Send + Sync>,
}

impl<R: Runtime> TauriInstaller<R> {
    pub fn new(
        app: AppHandle<R>,
        repository: Repository,
        public_key: impl Into<String>,
    ) -> Result<Self> {
        let public_key = public_key.into();
        decode_public_key(&public_key)?;
        Ok(Self {
            app,
            repository,
            public_key,
            timeout: Duration::from_secs(120),
            before_exit: Arc::new(|| {}),
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() || std::time::Instant::now().checked_add(timeout).is_none() {
            return Err(Error::InvalidConfig(
                "installer timeout must be positive and finite".into(),
            ));
        }
        self.timeout = timeout;
        Ok(self)
    }

    /// Windows: stop sidecars, flush state, and release single-instance locks.
    /// This is called by Tauri immediately before installer launch and process exit.
    pub fn on_before_exit(mut self, callback: impl Fn() + Send + Sync + 'static) -> Self {
        self.before_exit = Arc::new(callback);
        self
    }
}

#[async_trait]
impl<R: Runtime> InstallBackend for TauriInstaller<R> {
    async fn prepare(&self, release: &Release) -> Result<Box<dyn PreparedUpdate>> {
        let callback = self.before_exit.clone();
        // Pin to the selected tag. A newly published latest.json cannot change the
        // version being installed between the user's check and confirmation.
        let endpoint = self.repository.asset_url(&release.tag, "latest.json");
        let builder = self
            .app
            .updater_builder()
            .pubkey(&self.public_key)
            .timeout(self.timeout)
            .on_before_exit(move || callback())
            .endpoints(vec![endpoint])
            .map_err(backend_error)?;
        let mut update = builder
            .build()
            .map_err(backend_error)?
            .check()
            .await
            .map_err(backend_error)?
            .ok_or(Error::NoUpdate)?;
        if parse_version(&update.version)? != release.version {
            return Err(Error::Installer(
                "manifest version does not match the selected GitHub release".into(),
            ));
        }
        if update.download_url.scheme() != "https"
            || !update.download_url.username().is_empty()
            || update.download_url.password().is_some()
        {
            return Err(Error::Installer(
                "artifact URL must use HTTPS without credentials".into(),
            ));
        }
        // Tauri 2.11 applies the builder timeout to checks, but does not copy it to downloads.
        update.timeout = Some(self.timeout);
        Ok(Box::new(TauriUpdate(update)))
    }
}

struct TauriUpdate(Update);
#[async_trait]
impl PreparedUpdate for TauriUpdate {
    async fn download(&self, progress: DownloadCallback) -> Result<Vec<u8>> {
        let mut received = 0u64;
        self.0
            .download(
                move |chunk, total| {
                    received = received.saturating_add(chunk as u64);
                    progress(received, total);
                },
                || {},
            )
            .await
            .map_err(backend_error)
    }
    async fn install(&self, bytes: Vec<u8>) -> Result<()> {
        let update = self.0.clone();
        tokio::task::spawn_blocking(move || update.install(bytes))
            .await
            .map_err(|e| Error::Task(e.to_string()))?
            .map_err(backend_error)
    }
}

fn backend_error(error: impl std::fmt::Display) -> Error {
    Error::Installer(error.to_string())
}
