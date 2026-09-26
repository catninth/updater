use async_trait::async_trait;
use catninth_updater::*;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

fn release(version: &str) -> Release {
    Release {
        version: Version::parse(version).unwrap(),
        tag: format!("v{version}"),
        notes: Some("# Changes".into()),
        published_at: None,
    }
}
fn config() -> Config {
    Config::new(Repository::parse("catninth/clipcat").unwrap(), "1.0.0", 5).unwrap()
}

struct Source {
    results: Mutex<VecDeque<Result<Option<Release>>>>,
    calls: AtomicUsize,
}
impl Source {
    fn new(results: impl IntoIterator<Item = Result<Option<Release>>>) -> Arc<Self> {
        Arc::new(Self {
            results: Mutex::new(results.into_iter().collect()),
            calls: AtomicUsize::new(0),
        })
    }
}
#[async_trait]
impl ReleaseSource for Source {
    async fn latest_release(&self) -> Result<Option<Release>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Some(release("2.0.0"))))
    }
    async fn patch_notes(&self, version: &str) -> Result<Option<String>> {
        Ok(Some(format!("Notes for {version}")))
    }
}
fn updater(source: Arc<Source>) -> Updater {
    Updater::builder(config()).source(source).build().unwrap()
}

#[test]
fn config_rejects_invalid_interval_repository_and_version() {
    for repo in [
        "",
        "owner",
        "https://github.com/o/r",
        "o/r/x",
        "../repo",
        "owner/a?b",
        "o/r\n",
    ] {
        assert!(Repository::parse(repo).is_err(), "{repo}");
    }
    let repo = Repository::parse("catninth/clipcat").unwrap();
    assert!(Config::new(repo.clone(), "1.0.0", 0).is_err());
    assert!(Config::new(repo.clone(), "1.0.0", u64::MAX).is_err());
    assert!(Config::new(repo.clone(), "not-semver", 5).is_err());
    let config = Config::new(repo, "v1.2.3", 17).unwrap();
    assert_eq!(config.installed_version().to_string(), "1.2.3");
    assert_eq!(config.check_interval(), Duration::from_secs(17 * 60));
    assert!(config.with_request_timeout(Duration::ZERO).is_err());
}

#[tokio::test]
async fn successful_fetch_caches_notes_and_announces_each_version_once() {
    let source = Source::new([
        Ok(Some(release("2.0.0"))),
        Ok(Some(release("2.0.0"))),
        Ok(Some(release("3.0.0"))),
        Ok(Some(release("2.0.0"))),
    ]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = events.clone();
    let updater = Updater::builder(config())
        .source(source)
        .on_event(move |e| callback_events.lock().unwrap().push(e.clone()))
        .build()
        .unwrap();
    assert_eq!(updater.get_latest_version(), None);
    assert_eq!(updater.get_installed_version().to_string(), "1.0.0");
    for _ in 0..4 {
        updater.fetch_version().await.unwrap();
    }
    assert_eq!(
        updater.get_latest_version(),
        Some(Version::parse("2.0.0").unwrap())
    );
    assert_eq!(updater.state().phase, Phase::Available);
    assert_eq!(
        updater.get_patch_notes("1.0.0").await.unwrap().as_deref(),
        Some("Notes for 1.0.0")
    );
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::UpdateAvailable(_)))
            .count(),
        2
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::StateChanged(s) if s.phase == Phase::Checking))
    );
}

#[tokio::test]
async fn comparison_ignores_build_metadata_and_never_downgrades() {
    for version in ["1.0.0", "0.9.0", "1.0.0+build.2", "1.0.0-beta.1"] {
        let updater = updater(Source::new([Ok(Some(release(version)))]));
        updater.fetch_version().await.unwrap();
        assert_eq!(updater.state().phase, Phase::Latest, "{version}");
    }
    let updater = updater(Source::new([Ok(None)]));
    updater.fetch_version().await.unwrap();
    assert_eq!(updater.get_latest_version(), None);
    assert_eq!(updater.state().phase, Phase::Latest);
}

#[tokio::test]
async fn check_failure_preserves_cached_update_and_retry_clears_error() {
    let updater = updater(Source::new([
        Ok(Some(release("2.0.0"))),
        Err(Error::Http {
            status: 429,
            retry_after: Some("60".into()),
        }),
        Ok(None),
    ]));
    let mut events = updater.subscribe();
    updater.fetch_version().await.unwrap();
    assert!(updater.fetch_version().await.is_err());
    assert_eq!(updater.state().phase, Phase::Error);
    assert!(updater.get_latest_version().is_some());
    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).any(|e| matches!(
            e,
            Event::Failed {
                operation: Operation::Check,
                ..
            }
        ))
    );
    updater.fetch_version().await.unwrap();
    assert_eq!(updater.state().error, None);
    assert_eq!(updater.get_latest_version(), None);
}

#[derive(Default)]
struct Backend {
    downloads: AtomicUsize,
    installs: AtomicUsize,
    fail_download: AtomicBool,
    fail_install: AtomicBool,
    pause_download: AtomicBool,
    started: Notify,
    resume: Notify,
    recording: AtomicBool,
    record_during_download: AtomicBool,
    reserved: AtomicBool,
    after_calls: AtomicUsize,
}
#[derive(Clone)]
struct SharedBackend(Arc<Backend>);
impl std::ops::Deref for SharedBackend {
    type Target = Backend;
    fn deref(&self) -> &Backend {
        &self.0
    }
}
#[async_trait]
impl InstallBackend for SharedBackend {
    async fn prepare(&self, _: &Release) -> Result<Box<dyn PreparedUpdate>> {
        Ok(Box::new(self.clone()))
    }
}
#[async_trait]
impl PreparedUpdate for SharedBackend {
    async fn download(&self, progress: DownloadCallback) -> Result<Vec<u8>> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        if self.pause_download.load(Ordering::SeqCst) {
            self.resume.notified().await;
        }
        progress(5, None);
        progress(10, Some(10));
        if self.fail_download.load(Ordering::SeqCst) {
            return Err(Error::Installer("invalid signature".into()));
        }
        if self.record_during_download.load(Ordering::SeqCst) {
            self.recording.store(true, Ordering::SeqCst);
        }
        Ok(vec![1, 2, 3])
    }
    async fn install(&self, bytes: Vec<u8>) -> Result<()> {
        assert_eq!(bytes, [1, 2, 3]);
        assert!(self.reserved.load(Ordering::SeqCst));
        self.installs.fetch_add(1, Ordering::SeqCst);
        if self.fail_install.load(Ordering::SeqCst) {
            return Err(Error::Installer("disk is full".into()));
        }
        Ok(())
    }
}
struct Reservation(Arc<Backend>);
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.reserved.store(false, Ordering::SeqCst);
    }
}
#[async_trait]
impl InstallHooks for SharedBackend {
    async fn before_download(&self, _: &Release) -> Result<()> {
        if self.recording.load(Ordering::SeqCst) {
            return Err(Error::Blocked("recording".into()));
        }
        Ok(())
    }
    async fn before_install(&self, _: &Release) -> Result<Box<dyn InstallPermit>> {
        if self.recording.load(Ordering::SeqCst) {
            return Err(Error::Blocked("recording started while downloading".into()));
        }
        self.reserved.store(true, Ordering::SeqCst);
        Ok(Box::new(Reservation(self.0.clone())))
    }
    async fn after_install(&self, _: &Release) -> Result<()> {
        assert!(self.reserved.load(Ordering::SeqCst));
        self.after_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
async fn installable(backend: &Arc<Backend>) -> Updater {
    let updater = Updater::builder(config())
        .source(Source::new([]))
        .installer(Arc::new(SharedBackend(backend.clone())))
        .hooks(Arc::new(SharedBackend(backend.clone())))
        .build()
        .unwrap();
    updater.fetch_version().await.unwrap();
    updater
}

#[tokio::test]
async fn successful_install_reports_progress_retains_running_version_and_prevents_reinstall() {
    let backend = Arc::new(Backend::default());
    let updater = installable(&backend).await;
    let mut events = updater.subscribe();
    updater.install().await.unwrap();
    assert_eq!(updater.state().phase, Phase::Installed);
    assert_eq!(updater.get_installed_version().to_string(), "1.0.0");
    assert_eq!(
        updater.state().applied_version.unwrap().to_string(),
        "2.0.0"
    );
    assert_eq!(backend.after_calls.load(Ordering::SeqCst), 1);
    assert!(!backend.reserved.load(Ordering::SeqCst));
    assert!(matches!(updater.install().await, Err(Error::NoUpdate)));
    updater.fetch_version().await.unwrap();
    assert_eq!(updater.state().phase, Phase::Installed);
    let events: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::DownloadProgress(p) if p.percent.is_none()))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::DownloadProgress(p) if p.percent == Some(100)))
    );
    assert!(events.iter().any(|e| matches!(e, Event::Installed(_))));
}

#[tokio::test]
async fn both_recording_gates_block_installation() {
    for starts_during_download in [false, true] {
        let backend = Arc::new(Backend::default());
        backend
            .recording
            .store(!starts_during_download, Ordering::SeqCst);
        backend
            .record_during_download
            .store(starts_during_download, Ordering::SeqCst);
        let updater = installable(&backend).await;
        assert!(matches!(updater.install().await, Err(Error::Blocked(_))));
        assert_eq!(
            backend.downloads.load(Ordering::SeqCst),
            usize::from(starts_during_download)
        );
        assert_eq!(backend.installs.load(Ordering::SeqCst), 0);
        assert!(!backend.reserved.load(Ordering::SeqCst));
        assert_eq!(updater.state().phase, Phase::Error);
    }
}

#[tokio::test]
async fn download_and_install_failures_release_gate_and_allow_retry() {
    for download_failure in [true, false] {
        let backend = Arc::new(Backend::default());
        backend
            .fail_download
            .store(download_failure, Ordering::SeqCst);
        backend
            .fail_install
            .store(!download_failure, Ordering::SeqCst);
        let updater = installable(&backend).await;
        let mut events = updater.subscribe();
        assert!(updater.install().await.is_err());
        assert_eq!(updater.state().phase, Phase::Error);
        assert!(updater.get_latest_version().is_some());
        assert!(!backend.reserved.load(Ordering::SeqCst));
        let operation = if download_failure {
            Operation::Download
        } else {
            Operation::Install
        };
        assert!(
            std::iter::from_fn(|| events.try_recv().ok())
                .any(|e| matches!(e, Event::Failed { operation: o, .. } if o == operation))
        );
        backend.fail_download.store(false, Ordering::SeqCst);
        backend.fail_install.store(false, Ordering::SeqCst);
        updater.install().await.unwrap();
        assert_eq!(updater.state().error, None);
    }
}

#[tokio::test]
async fn dropped_install_waiter_does_not_release_gate_during_installation() {
    let backend = Arc::new(Backend::default());
    backend.pause_download.store(true, Ordering::SeqCst);
    let updater = installable(&backend).await;
    let mut events = updater.subscribe();
    let worker = {
        let updater = updater.clone();
        tokio::spawn(async move { updater.install().await })
    };
    backend.started.notified().await;
    assert!(matches!(updater.fetch_version().await, Err(Error::Busy)));
    assert!(matches!(updater.install().await, Err(Error::Busy)));
    worker.abort();
    let _ = worker.await;
    assert!(matches!(updater.fetch_version().await, Err(Error::Busy)));
    backend.resume.notify_one();
    loop {
        if matches!(events.recv().await.unwrap(), Event::Installed(_)) {
            break;
        }
    }
    assert_eq!(backend.installs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_backend_and_missing_release_do_not_stick_busy() {
    let updater = updater(Source::new([]));
    assert!(matches!(updater.install().await, Err(Error::NoInstaller)));
    updater.fetch_version().await.unwrap();
    let backend = Arc::new(Backend::default());
    let updater = Updater::builder(config())
        .source(Source::new([]))
        .installer(Arc::new(SharedBackend(backend)))
        .build()
        .unwrap();
    assert!(matches!(updater.install().await, Err(Error::NoUpdate)));
    updater.fetch_version().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn polling_obeys_delay_minutes_recovers_from_errors_and_stops() {
    let source = Source::new([
        Err(Error::Installer("offline".into())),
        Ok(Some(release("2.0.0"))),
    ]);
    let updater = updater(source.clone());
    let polling = updater.start().unwrap();
    assert!(matches!(updater.start(), Err(Error::AlreadyPolling)));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(19)).await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_eq!(updater.state().phase, Phase::Error);
    tokio::time::advance(Duration::from_secs(299)).await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    polling.stop().await.unwrap();
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    updater.start().unwrap().stop().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn dropping_polling_handle_stops_and_callback_panics_do_not_break_state() {
    let source = Source::new([]);
    let updater = Updater::builder(config())
        .source(source.clone())
        .on_event(|_| panic!("consumer callback"))
        .build()
        .unwrap();
    updater.fetch_version().await.unwrap();
    let handle = updater.start().unwrap();
    drop(handle);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    updater.start().unwrap().stop().await.unwrap();
}

#[test]
fn progress_handles_unknown_zero_oversized_and_overflowing_counts() {
    assert_eq!(DownloadProgress::new(10, None).percent, None);
    assert_eq!(DownloadProgress::new(10, Some(0)).percent, None);
    assert_eq!(DownloadProgress::new(20, Some(10)).percent, Some(100));
    assert_eq!(
        DownloadProgress::new(u64::MAX, Some(u64::MAX)).percent,
        Some(100)
    );
    assert_eq!(DownloadProgress::new(1, Some(3)).percent, Some(33));
}

struct PausedSource {
    started: Notify,
    resume: Notify,
}
#[async_trait]
impl ReleaseSource for PausedSource {
    async fn latest_release(&self) -> Result<Option<Release>> {
        self.started.notify_one();
        self.resume.notified().await;
        Ok(Some(release("2.0.0")))
    }
    async fn patch_notes(&self, _: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

#[tokio::test]
async fn concurrent_check_and_install_are_excluded_while_fetch_is_pending() {
    let source = Arc::new(PausedSource {
        started: Notify::new(),
        resume: Notify::new(),
    });
    let updater = Updater::builder(config())
        .source(source.clone())
        .build()
        .unwrap();
    let worker = {
        let updater = updater.clone();
        tokio::spawn(async move { updater.fetch_version().await })
    };
    source.started.notified().await;
    assert_eq!(updater.state().phase, Phase::Checking);
    assert!(matches!(updater.fetch_version().await, Err(Error::Busy)));
    assert!(matches!(updater.install().await, Err(Error::Busy)));
    source.resume.notify_one();
    worker.await.unwrap().unwrap();
    assert_eq!(updater.state().phase, Phase::Available);
}

struct PanickingSource(AtomicBool);
#[async_trait]
impl ReleaseSource for PanickingSource {
    async fn latest_release(&self) -> Result<Option<Release>> {
        assert!(!self.0.swap(false, Ordering::SeqCst), "source panic");
        Ok(Some(release("2.0.0")))
    }
    async fn patch_notes(&self, _: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

#[tokio::test]
async fn panicking_source_marks_failure_and_releases_operation_gate() {
    let updater = Updater::builder(config())
        .source(Arc::new(PanickingSource(AtomicBool::new(true))))
        .build()
        .unwrap();
    assert!(matches!(updater.fetch_version().await, Err(Error::Task(_))));
    assert_eq!(updater.state().phase, Phase::Error);
    updater.fetch_version().await.unwrap();
    assert_eq!(updater.state().phase, Phase::Available);
}
