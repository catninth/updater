use std::time::Duration;

use semver::Version;
use url::Url;

use crate::{Error, Result};

/// A GitHub repository in `owner/name` form. No credentials or URLs are accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    owner: String,
    name: String,
}

impl Repository {
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Result<Self> {
        let repository = Self {
            owner: owner.into(),
            name: name.into(),
        };
        for value in [&repository.owner, &repository.name] {
            if value.is_empty()
                || value == "."
                || value == ".."
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            {
                return Err(Error::InvalidConfig("repository must be owner/name".into()));
            }
        }
        Ok(repository)
    }

    pub fn parse(value: &str) -> Result<Self> {
        let (owner, name) = value
            .split_once('/')
            .ok_or_else(|| Error::InvalidConfig("repository must be owner/name".into()))?;
        Self::new(owner, name)
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn latest_manifest_url(&self) -> Url {
        self.github_url(&["releases", "latest", "download", "latest.json"])
    }

    /// Each component is URL encoded, including tags containing `/` or `+`.
    pub fn asset_url(&self, tag: &str, asset: &str) -> Url {
        self.github_url(&["releases", "download", tag, asset])
    }

    fn github_url(&self, segments: &[&str]) -> Url {
        let mut url = Url::parse("https://github.com").expect("constant URL");
        url.path_segments_mut()
            .expect("HTTPS base")
            .extend([self.owner(), self.name()])
            .extend(segments);
        url
    }
}

impl std::fmt::Display for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// The installed version belongs to the consumer, never this library's package.
#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) repository: Repository,
    pub(crate) installed_version: Version,
    pub(crate) check_interval: Duration,
    pub(crate) first_check_delay: Duration,
    pub(crate) request_timeout: Duration,
}

impl Config {
    pub fn new(
        repository: Repository,
        installed_version: &str,
        interval_minutes: u64,
    ) -> Result<Self> {
        let seconds = interval_minutes
            .checked_mul(60)
            .filter(|s| *s > 0)
            .ok_or_else(|| {
                Error::InvalidConfig("interval_minutes must be positive and fit in seconds".into())
            })?;
        let check_interval = Duration::from_secs(seconds);
        if std::time::Instant::now()
            .checked_add(check_interval)
            .is_none()
        {
            return Err(Error::InvalidConfig(
                "check interval exceeds the system clock range".into(),
            ));
        }
        Ok(Self {
            repository,
            installed_version: parse_version(installed_version)?,
            check_interval,
            first_check_delay: Duration::from_secs(20),
            request_timeout: Duration::from_secs(120),
        })
    }

    pub fn with_first_check_delay(mut self, delay: Duration) -> Result<Self> {
        if std::time::Instant::now().checked_add(delay).is_none() {
            return Err(Error::InvalidConfig(
                "first check delay exceeds the system clock range".into(),
            ));
        }
        self.first_check_delay = delay;
        Ok(self)
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() || std::time::Instant::now().checked_add(timeout).is_none() {
            return Err(Error::InvalidConfig(
                "request timeout must be positive and finite".into(),
            ));
        }
        self.request_timeout = timeout;
        Ok(self)
    }

    pub fn repository(&self) -> &Repository {
        &self.repository
    }
    pub fn installed_version(&self) -> &Version {
        &self.installed_version
    }
    pub fn check_interval(&self) -> Duration {
        self.check_interval
    }
    pub fn first_check_delay(&self) -> Duration {
        self.first_check_delay
    }
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }
}

pub(crate) fn parse_version(value: &str) -> Result<Version> {
    Version::parse(value.strip_prefix('v').unwrap_or(value))
        .map_err(|_| Error::InvalidVersion(value.into()))
}
