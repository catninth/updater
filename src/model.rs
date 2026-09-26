use semver::Version;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// GitHub release body, in Markdown; this is not the repository README.
    pub notes: Option<String>,
    pub published_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Idle,
    Checking,
    Latest,
    Available,
    Downloading,
    Installing,
    Installed,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub received: u64,
    pub total: Option<u64>,
    pub percent: Option<u8>,
}

impl DownloadProgress {
    pub fn new(received: u64, total: Option<u64>) -> Self {
        let total = total.filter(|n| *n > 0);
        let percent = total.map(|n| ((received as u128 * 100 / n as u128).min(100)) as u8);
        Self {
            received,
            total,
            percent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub phase: Phase,
    pub current: Version,
    pub latest: Option<Release>,
    pub progress: Option<DownloadProgress>,
    pub error: Option<String>,
    /// Successfully installed on disk; the running process may still be older.
    pub applied_version: Option<Version>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Operation {
    Check,
    PatchNotes,
    Download,
    Install,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "camelCase")]
pub enum Event {
    StateChanged(UpdateState),
    /// Emitted once per version per Updater lifetime (state updates still repeat).
    UpdateAvailable(Release),
    UpToDate,
    DownloadProgress(DownloadProgress),
    Installed(Release),
    Failed {
        operation: Operation,
        message: String,
    },
}
