use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use tokio::sync::{Semaphore, broadcast, oneshot};

use crate::{
    Config, DownloadProgress, Error, Event, Operation, Phase, Release, Result, UpdateState,
    Version, github::GitHubSource,
};

#[async_trait]
pub trait ReleaseSource: Send + Sync + 'static {
    async fn latest_release(&self) -> Result<Option<Release>>;
    async fn patch_notes(&self, version: &str) -> Result<Option<String>>;
}

/// Callback receives cumulative bytes, not the size of an individual chunk.
pub type DownloadCallback = Arc<dyn Fn(u64, Option<u64>) + Send + Sync>;

#[async_trait]
pub trait InstallBackend: Send + Sync + 'static {
    /// Resolve and validate a package for this exact release and host platform.
    async fn prepare(&self, release: &Release) -> Result<Box<dyn PreparedUpdate>>;
}

#[async_trait]
pub trait PreparedUpdate: Send + Sync {
    /// Implementations MUST verify the artifact signature before returning bytes.
    async fn download(&self, progress: DownloadCallback) -> Result<Vec<u8>>;
    /// Runs only after the final consumer installation gate is acquired.
    async fn install(&self, bytes: Vec<u8>) -> Result<()>;
}

/// A consumer reservation held through install and `after_install`.
/// Implement Drop to release it on failure. Windows process exit does not run Drop.
pub trait InstallPermit: Send {}
impl<T: Send> InstallPermit for T {}

/// App-specific work, such as stopping recording or releasing a single-instance lock.
#[async_trait]
pub trait InstallHooks: Send + Sync + 'static {
    async fn before_download(&self, _release: &Release) -> Result<()> {
        Ok(())
    }

    /// Recheck after downloading and atomically reserve the application for install.
    /// Capture starts and this method must share the same consumer operation gate.
    async fn before_install(&self, _release: &Release) -> Result<Box<dyn InstallPermit>> {
        Ok(Box::new(()))
    }

    /// Linux/macOS: release app locks and optionally restart here.
    /// Windows installers normally exit the process, so use the adapter's before-exit hook.
    async fn after_install(&self, _release: &Release) -> Result<()> {
        Ok(())
    }
}

struct NoHooks;
#[async_trait]
impl InstallHooks for NoHooks {}

type EventCallback = Arc<dyn Fn(&Event) + Send + Sync>;

pub struct UpdaterBuilder {
    config: Config,
    source: Option<Arc<dyn ReleaseSource>>,
    installer: Option<Arc<dyn InstallBackend>>,
    hooks: Arc<dyn InstallHooks>,
    callbacks: Vec<EventCallback>,
}

impl UpdaterBuilder {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            source: None,
            installer: None,
            hooks: Arc::new(NoHooks),
            callbacks: Vec::new(),
        }
    }
    pub fn source(mut self, source: Arc<dyn ReleaseSource>) -> Self {
        self.source = Some(source);
        self
    }
    pub fn installer(mut self, installer: Arc<dyn InstallBackend>) -> Self {
        self.installer = Some(installer);
        self
    }
    pub fn hooks(mut self, hooks: Arc<dyn InstallHooks>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Runs synchronously outside state locks. Keep callbacks short; unwinding panics
    /// are isolated (a consumer compiled with panic=abort still aborts the process).
    pub fn on_event(mut self, callback: impl Fn(&Event) + Send + Sync + 'static) -> Self {
        self.callbacks.push(Arc::new(callback));
        self
    }

    pub fn build(self) -> Result<Updater> {
        let source = match self.source {
            Some(source) => source,
            None => Arc::new(GitHubSource::new(
                self.config.repository.clone(),
                self.config.request_timeout,
            )?),
        };
        let state = UpdateState {
            phase: Phase::Idle,
            current: self.config.installed_version.clone(),
            latest: None,
            progress: None,
            error: None,
            applied_version: None,
        };
        Ok(Updater(Arc::new(Inner {
            config: self.config,
            source,
            installer: self.installer,
            hooks: self.hooks,
            state: Mutex::new(state),
            announced: Mutex::new(HashSet::new()),
            operation: Arc::new(Semaphore::new(1)),
            polling: AtomicBool::new(false),
            events: broadcast::channel(128).0,
            callbacks: self.callbacks,
        })))
    }
}

struct Inner {
    config: Config,
    source: Arc<dyn ReleaseSource>,
    installer: Option<Arc<dyn InstallBackend>>,
    hooks: Arc<dyn InstallHooks>,
    state: Mutex<UpdateState>,
    announced: Mutex<HashSet<Version>>,
    operation: Arc<Semaphore>,
    polling: AtomicBool,
    events: broadcast::Sender<Event>,
    callbacks: Vec<EventCallback>,
}

/// Cloneable handle. All clones share state, polling, and a single operation gate.
#[derive(Clone)]
pub struct Updater(Arc<Inner>);

impl Updater {
    pub fn builder(config: Config) -> UpdaterBuilder {
        UpdaterBuilder::new(config)
    }
    pub fn config(&self) -> &Config {
        &self.0.config
    }

    /// Version of the running consumer, unchanged until it restarts.
    pub fn get_installed_version(&self) -> Version {
        self.0.config.installed_version.clone()
    }

    /// Cached value; `None` before the first successful fetch or when no release exists.
    pub fn get_latest_version(&self) -> Option<Version> {
        self.state().latest.map(|release| release.version)
    }

    pub fn state(&self) -> UpdateState {
        self.0.state.lock().expect("state lock").clone()
    }

    /// Subscribe before starting work. If lagged, recover a snapshot via `state()`.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }

    /// Fetch the latest stable release, compare SemVer precedence, and update the cache.
    /// Busy is returned rather than allowing checks to overlap an installation.
    /// Dropping the awaiting future does not cancel an operation already started.
    pub async fn fetch_version(&self) -> Result<Option<Release>> {
        let permit = self
            .0
            .operation
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let this = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut guard = OperationGuard::new(this.clone(), Operation::Check);
            this.change(|state| {
                state.phase = Phase::Checking;
                state.error = None;
                state.progress = None;
            });
            let result = this.fetch_inner().await;
            if let Err(error) = &result {
                this.fail(Operation::Check, error);
            }
            guard.complete = true;
            result
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?
    }

    async fn fetch_inner(&self) -> Result<Option<Release>> {
        let latest = self.0.source.latest_release().await?;
        let state = self.state();
        let baseline = state.applied_version.as_ref().unwrap_or(&state.current);
        let available = latest
            .as_ref()
            .is_some_and(|release| newer(&release.version, baseline));
        self.change(|state| {
            state.latest = latest.clone();
            state.phase = if available {
                Phase::Available
            } else if state.applied_version.is_some() {
                Phase::Installed
            } else {
                Phase::Latest
            };
        });
        if let Some(release) = latest.as_ref().filter(|_| available) {
            let first = self
                .0
                .announced
                .lock()
                .expect("announcement lock")
                .insert(release.version.clone());
            if first {
                self.emit(Event::UpdateAvailable(release.clone()));
            }
        } else {
            self.emit(Event::UpToDate);
        }
        Ok(latest)
    }

    /// Fetch a particular release's Markdown body. Works for installed/older versions.
    pub async fn get_patch_notes(&self, version: &str) -> Result<Option<String>> {
        let result = self.0.source.patch_notes(version).await;
        if let Err(error) = &result {
            // A notes-panel request must not overwrite an ongoing install's state.
            self.emit(Event::Failed {
                operation: Operation::PatchNotes,
                message: error.to_string(),
            });
        }
        result
    }

    /// Download a signed update and install only after both consumer gates succeed.
    /// Once started this operation continues even if its caller drops the future.
    /// Keep the Tokio runtime alive until completion; restarting is consumer-controlled.
    pub async fn install(&self) -> Result<()> {
        let permit = self
            .0
            .operation
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let backend = self.0.installer.clone().ok_or(Error::NoInstaller)?;
        let state = self.state();
        let baseline = state.applied_version.as_ref().unwrap_or(&state.current);
        let release = state
            .latest
            .filter(|release| newer(&release.version, baseline))
            .ok_or(Error::NoUpdate)?;
        let this = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut guard = OperationGuard::new(this.clone(), Operation::Install);
            let result = this.install_inner(backend, &release).await;
            if let Err(error) = &result {
                let operation = if this.state().phase == Phase::Downloading {
                    Operation::Download
                } else {
                    Operation::Install
                };
                this.fail(operation, error);
            }
            guard.complete = true;
            result
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?
    }

    async fn install_inner(
        &self,
        backend: Arc<dyn InstallBackend>,
        release: &Release,
    ) -> Result<()> {
        self.0.hooks.before_download(release).await?;
        self.change(|state| {
            state.phase = Phase::Downloading;
            state.error = None;
            state.progress = Some(DownloadProgress::new(0, None));
        });
        let prepared = backend.prepare(release).await?;
        let this = self.clone();
        let bytes = prepared
            .download(Arc::new(move |received, total| {
                let progress = DownloadProgress::new(received, total);
                this.change(|state| state.progress = Some(progress));
                this.emit(Event::DownloadProgress(progress));
            }))
            .await?;
        // No file changes have happened yet. Reserve against competing consumer work.
        self.change(|state| {
            state.phase = Phase::Installing;
            state.progress = None;
        });
        let _reservation = self.0.hooks.before_install(release).await?;
        prepared.install(bytes).await?;
        self.change(|state| {
            state.phase = Phase::Installed;
            state.applied_version = Some(release.version.clone());
        });
        self.emit(Event::Installed(release.clone()));
        self.0.hooks.after_install(release).await
    }

    /// Start one polling loop. First delay defaults to 20 seconds; subsequent checks
    /// wait the configured number of minutes after the preceding check completes.
    /// Retain the returned handle: dropping it stops polling, but not an active install.
    pub fn start(&self) -> Result<PollingHandle> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| Error::InvalidConfig("start requires a Tokio runtime".into()))?;
        self.0
            .polling
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| Error::AlreadyPolling)?;
        let (stop, mut stopped) = oneshot::channel();
        let this = self.clone();
        let guard = PollingGuard(this.clone());
        let task = runtime.spawn(async move {
            let _guard = guard;
            let mut delay = this.0.config.first_check_delay;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    _ = tokio::time::sleep(delay) => {}
                }
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    _ = this.fetch_version() => {}
                }
                delay = this.0.config.check_interval;
            }
        });
        Ok(PollingHandle {
            stop: Some(stop),
            task: Some(task),
        })
    }

    fn change(&self, change: impl FnOnce(&mut UpdateState)) {
        let state = {
            let mut state = self.0.state.lock().expect("state lock");
            change(&mut state);
            state.clone()
        };
        self.emit(Event::StateChanged(state));
    }

    fn emit(&self, event: Event) {
        let _ = self.0.events.send(event.clone());
        for callback in &self.0.callbacks {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(&event)));
        }
    }

    fn fail(&self, operation: Operation, error: &Error) {
        self.change(|state| {
            state.phase = Phase::Error;
            state.progress = None;
            state.error = Some(error.to_string());
        });
        self.emit(Event::Failed {
            operation,
            message: error.to_string(),
        });
    }
}

fn newer(candidate: &Version, current: &Version) -> bool {
    candidate.cmp_precedence(current).is_gt()
}

struct OperationGuard {
    updater: Updater,
    operation: Operation,
    complete: bool,
}
impl OperationGuard {
    fn new(updater: Updater, operation: Operation) -> Self {
        Self {
            updater,
            operation,
            complete: false,
        }
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.updater
                .fail(self.operation, &Error::Task("operation interrupted".into()));
        }
    }
}

struct PollingGuard(Updater);
impl Drop for PollingGuard {
    fn drop(&mut self) {
        self.0.0.polling.store(false, Ordering::SeqCst);
    }
}

pub struct PollingHandle {
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl PollingHandle {
    /// Stops future checks and waits for the polling loop to end. An already
    /// started request can finish independently, keeping the shared gate until done.
    pub async fn stop(mut self) -> Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.map_err(|e| Error::Task(e.to_string()))?;
        }
        Ok(())
    }
}
impl Drop for PollingHandle {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
