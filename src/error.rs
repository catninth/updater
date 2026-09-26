pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid release version: {0}")]
    InvalidVersion(String),
    #[error("GitHub returned HTTP {status} (retry after: {retry_after:?})")]
    Http {
        status: u16,
        retry_after: Option<String>,
    },
    #[error("network request failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("invalid release response: {0}")]
    Response(#[from] serde_json::Error),
    #[error("an update operation is already running")]
    Busy,
    #[error("the polling loop is already running")]
    AlreadyPolling,
    #[error("no newer release is available to install")]
    NoUpdate,
    #[error("no installer backend is configured")]
    NoInstaller,
    #[error("installer failed: {0}")]
    Installer(String),
    #[error("installation blocked: {0}")]
    Blocked(String),
    #[error("background operation failed: {0}")]
    Task(String),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("packaging command failed with exit code {0:?}")]
    Command(Option<i32>),
}
