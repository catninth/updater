//! Public GitHub release metadata without authenticated or rate-limited API requests.
//! Installer signatures are verified by the installation backend, not this source.

use std::{collections::BTreeMap, time::Duration};

use async_trait::async_trait;
use reqwest::{Client, StatusCode, header, redirect};
use serde::Deserialize;
use url::Url;

use crate::{
    Error, Release, ReleaseSource, Repository, Result, config::parse_version,
    installer::ManifestArtifact,
};

/// Reads `latest.json` assets from public GitHub releases, without using the REST API.
/// Every artifact must belong to one matching release in the configured repository.
pub struct GitHubManifestSource {
    repository: Repository,
    client: Client,
    base_url: Url,
}

impl GitHubManifestSource {
    pub fn new(repository: Repository, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() || std::time::Instant::now().checked_add(timeout).is_none() {
            return Err(Error::InvalidConfig(
                "request timeout must be positive and finite".into(),
            ));
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(Self {
            repository,
            client: Client::builder()
                .timeout(timeout)
                .user_agent(concat!("catninth-updater/", env!("CARGO_PKG_VERSION")))
                .redirect(redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= 10 {
                        attempt.error("too many manifest redirects")
                    } else if !safe_transport_url(attempt.url()) {
                        attempt.error("manifest redirect must use HTTPS without credentials")
                    } else {
                        attempt.follow()
                    }
                }))
                .build()?,
            base_url: Url::parse("https://github.com/").expect("constant URL"),
        })
    }

    /// Override the public release download base, for example a GitHub asset mirror.
    /// Artifact URLs inside manifests must still identify the configured GitHub repository.
    /// HTTP is accepted only for explicitly configured loopback test servers.
    pub fn with_base_url(mut self, mut url: Url) -> Result<Self> {
        if !safe_transport_url(&url) || url.query().is_some() || url.fragment().is_some() {
            return Err(Error::InvalidConfig(
                "manifest base must be HTTPS without credentials, query or fragment".into(),
            ));
        }
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        self.base_url = url;
        Ok(self)
    }

    async fn request(&self, tag: Option<&str>) -> Result<Option<Release>> {
        let github_url = match tag {
            Some(tag) => self.repository.asset_url(tag, "latest.json"),
            None => self.repository.latest_manifest_url(),
        };
        let url = self
            .base_url
            .join(github_url.path().trim_start_matches('/'))
            .map_err(|_| Error::InvalidConfig("invalid manifest base URL".into()))?;
        let response = self
            .client
            .get(url)
            .header(header::ACCEPT, "application/json")
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Error::Http {
                status: response.status().as_u16(),
                retry_after: response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            });
        }
        let manifest: Manifest = serde_json::from_slice(&response.bytes().await?)?;
        let release = self.release(manifest)?;
        if let Some(tag) = tag
            && release.tag != tag
        {
            return Err(Error::InvalidConfig(
                "manifest release does not match the requested tag".into(),
            ));
        }
        Ok(Some(release))
    }

    fn release(&self, manifest: Manifest) -> Result<Release> {
        let version = parse_version(&manifest.version)?;
        if manifest.platforms.is_empty() {
            return Err(Error::InvalidConfig(
                "release manifest has no platforms".into(),
            ));
        }
        // GitHub redirects assets to a CDN with an opaque URL. Recover the exact
        // supported tag from the manifest instead of guessing its optional v prefix.
        let candidates = [version.to_string(), format!("v{version}")];
        let tag = candidates
            .into_iter()
            .find(|tag| {
                let endpoint = self.repository.asset_url(tag, "latest.json");
                let prefix = endpoint
                    .as_str()
                    .strip_suffix("latest.json")
                    .expect("constant asset name");
                manifest.platforms.values().all(|artifact| {
                    let url = &artifact.url;
                    url.scheme() == "https"
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none()
                        && url
                            .as_str()
                            .strip_prefix(prefix)
                            .is_some_and(|name| !name.is_empty() && !name.contains('/'))
                })
            })
            .ok_or_else(|| {
                Error::InvalidConfig(
                    "manifest artifacts must share a matching GitHub repository and release tag"
                        .into(),
                )
            })?;
        Ok(Release {
            version,
            tag,
            notes: manifest
                .notes
                .map(|notes| notes.trim().to_owned())
                .filter(|notes| !notes.is_empty()),
            published_at: manifest.pub_date,
        })
    }
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    notes: Option<String>,
    pub_date: Option<String>,
    platforms: BTreeMap<String, ManifestArtifact>,
}

#[async_trait]
impl ReleaseSource for GitHubManifestSource {
    async fn latest_release(&self) -> Result<Option<Release>> {
        Ok(self
            .request(None)
            .await?
            .filter(|release| release.version.pre.is_empty()))
    }

    async fn patch_notes(&self, version: &str) -> Result<Option<String>> {
        if version.trim().is_empty() {
            return Err(Error::InvalidConfig(
                "release version or tag is empty".into(),
            ));
        }
        if let Some(release) = self.request(Some(version)).await? {
            return Ok(release.notes);
        }
        if parse_version(version).is_ok() {
            let alternate = version
                .strip_prefix('v')
                .map(str::to_owned)
                .unwrap_or_else(|| format!("v{version}"));
            if let Some(release) = self.request(Some(&alternate)).await? {
                return Ok(release.notes);
            }
        }
        Ok(None)
    }
}

fn safe_transport_url(url: &Url) -> bool {
    let loopback = match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    (url.scheme() == "https" || url.scheme() == "http" && loopback)
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}
