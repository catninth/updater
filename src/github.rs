//! GitHub REST API release metadata. Stable releases only; nightly tags are ignored.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use url::Url;

use crate::{Error, Release, ReleaseSource, Repository, Result, config::parse_version};

pub struct GitHubSource {
    repository: Repository,
    client: Client,
    api_url: Url,
    authorization: Option<header::HeaderValue>,
}

impl GitHubSource {
    pub fn new(repository: Repository, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() || std::time::Instant::now().checked_add(timeout).is_none() {
            return Err(Error::InvalidConfig(
                "request timeout must be positive".into(),
            ));
        }
        // Match the Tauri adapter's provider without replacing a consumer's choice.
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(Self {
            repository,
            client: Client::builder()
                .timeout(timeout)
                .user_agent(concat!("catninth-updater/", env!("CARGO_PKG_VERSION")))
                .build()?,
            api_url: Url::parse("https://api.github.com/").expect("constant URL"),
            authorization: None,
        })
    }

    /// Token is used only for metadata requests, never installer download URLs.
    pub fn with_token(mut self, token: &str) -> Result<Self> {
        if token.trim().is_empty() {
            return Err(Error::InvalidConfig("GitHub token is empty".into()));
        }
        let mut value = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Error::InvalidConfig("invalid GitHub token header".into()))?;
        value.set_sensitive(true);
        self.authorization = Some(value);
        Ok(self)
    }

    /// Enterprise API base, e.g. `https://git.example.com/api/v3/`.
    /// HTTP is accepted only for explicitly configured loopback test servers.
    pub fn with_api_url(mut self, url: Url) -> Result<Self> {
        let loopback = match url.host() {
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::InvalidConfig(
                "GitHub API base must be HTTPS without credentials, query or fragment".into(),
            ));
        }
        self.api_url = url;
        Ok(self)
    }

    async fn request(&self, tail: &[&str]) -> Result<Option<ApiRelease>> {
        let mut url = self.api_url.clone();
        url.path_segments_mut()
            .map_err(|_| Error::InvalidConfig("invalid GitHub API base".into()))?
            .pop_if_empty()
            .extend([
                "repos",
                self.repository.owner(),
                self.repository.name(),
                "releases",
            ])
            .extend(tail);
        let mut request = self
            .client
            .get(url)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.authorization {
            request = request.header(header::AUTHORIZATION, token.clone());
        }
        let response = request.send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Error::Http {
                status: response.status().as_u16(),
                retry_after: response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .map(str::to_owned),
            });
        }
        Ok(Some(serde_json::from_slice(&response.bytes().await?)?))
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    body: Option<String>,
    published_at: Option<String>,
    draft: bool,
    prerelease: bool,
}

#[async_trait]
impl ReleaseSource for GitHubSource {
    async fn latest_release(&self) -> Result<Option<Release>> {
        let Some(release) = self.request(&["latest"]).await? else {
            return Ok(None);
        };
        if release.draft || release.prerelease {
            return Ok(None);
        }
        let version = parse_version(&release.tag_name)?;
        if !version.pre.is_empty() {
            return Ok(None);
        }
        Ok(Some(Release {
            version,
            tag: release.tag_name,
            notes: clean_notes(release.body),
            published_at: release.published_at,
        }))
    }

    async fn patch_notes(&self, version: &str) -> Result<Option<String>> {
        if version.trim().is_empty() {
            return Err(Error::InvalidConfig(
                "release version or tag is empty".into(),
            ));
        }
        if let Some(release) = self.request(&["tags", version]).await? {
            return Ok(if release.draft {
                None
            } else {
                clean_notes(release.body)
            });
        }
        // Accept both `1.2.3` and `v1.2.3`, while preserving arbitrary exact tags.
        if parse_version(version).is_ok() {
            let alternate = version
                .strip_prefix('v')
                .map(str::to_owned)
                .unwrap_or_else(|| format!("v{version}"));
            if let Some(release) = self.request(&["tags", &alternate]).await? {
                return Ok(if release.draft {
                    None
                } else {
                    clean_notes(release.body)
                });
            }
        }
        Ok(None)
    }
}

fn clean_notes(notes: Option<String>) -> Option<String> {
    notes.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}
