//! Native Tauri package building, artifact signature verification, and release manifests.
//! Build on the target OS using an installed Tauri 2 CLI. No shell strings are executed.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::{Error, Repository, Result, Version, config::parse_version};

/// Verify Tauri's base64-encoded minisign key/signature against the exact artifact bytes.
pub fn verify_artifact(bytes: &[u8], signature: &str, public_key: &str) -> Result<()> {
    let key = decode_public_key(public_key)?;
    let signature = decode_signature(signature)?;
    key.verify(bytes, &signature, true)
        .map_err(|e| Error::Installer(format!("signature verification failed: {e}")))
}

fn decode_text(value: &str) -> Result<String> {
    String::from_utf8(
        STANDARD
            .decode(value.trim())
            .map_err(|_| Error::InvalidConfig("invalid base64 signing data".into()))?,
    )
    .map_err(|_| Error::InvalidConfig("signing data must contain UTF-8 minisign text".into()))
}
pub(crate) fn decode_public_key(key: &str) -> Result<PublicKey> {
    PublicKey::decode(&decode_text(key)?)
        .map_err(|e| Error::InvalidConfig(format!("invalid public key: {e}")))
}
fn decode_signature(signature: &str) -> Result<Signature> {
    Signature::decode(&decode_text(signature)?)
        .map_err(|e| Error::InvalidConfig(format!("invalid signature: {e}")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatingSystem {
    Windows,
    Linux,
    MacOs,
}
impl OperatingSystem {
    pub fn current() -> Result<Self> {
        match std::env::consts::OS {
            "windows" => Ok(Self::Windows),
            "linux" => Ok(Self::Linux),
            "macos" => Ok(Self::MacOs),
            os => Err(Error::InvalidConfig(format!(
                "unsupported desktop OS: {os}"
            ))),
        }
    }
    pub fn manifest_name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::MacOs => "darwin",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Architecture {
    X86_64,
    Aarch64,
    I686,
    Armv7,
}
impl Architecture {
    pub fn manifest_name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::I686 => "i686",
            Self::Armv7 => "armv7",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageFormat {
    Nsis,
    Msi,
    AppImage,
    Deb,
    Rpm,
    App,
    Dmg,
}
impl PackageFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nsis => "nsis",
            Self::Msi => "msi",
            Self::AppImage => "appimage",
            Self::Deb => "deb",
            Self::Rpm => "rpm",
            Self::App => "app",
            Self::Dmg => "dmg",
        }
    }
    pub fn os(self) -> OperatingSystem {
        match self {
            Self::Nsis | Self::Msi => OperatingSystem::Windows,
            Self::AppImage | Self::Deb | Self::Rpm => OperatingSystem::Linux,
            Self::App | Self::Dmg => OperatingSystem::MacOs,
        }
    }
    fn matches_artifact(self, name: &str) -> bool {
        name.ends_with(match self {
            Self::Nsis => ".exe",
            Self::Msi => ".msi",
            Self::AppImage => ".AppImage",
            Self::Deb => ".deb",
            Self::Rpm => ".rpm",
            Self::App => ".app.tar.gz",
            Self::Dmg => ".dmg",
        })
    }
}

/// Use a format-specific target for deb/rpm so the updater preserves installation type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platform {
    os: OperatingSystem,
    architecture: Architecture,
    format: Option<PackageFormat>,
}
impl Platform {
    pub fn new(os: OperatingSystem, architecture: Architecture) -> Self {
        Self {
            os,
            architecture,
            format: None,
        }
    }
    pub fn with_format(mut self, format: PackageFormat) -> Result<Self> {
        if format.os() != self.os || format == PackageFormat::Dmg {
            return Err(Error::InvalidConfig(
                "package format is not an update artifact for this OS (macOS uses app.tar.gz)"
                    .into(),
            ));
        }
        self.format = Some(format);
        Ok(self)
    }
    pub fn key(self) -> String {
        let base = format!(
            "{}-{}",
            self.os.manifest_name(),
            self.architecture.manifest_name()
        );
        self.format
            .map(|format| format!("{base}-{}", format.as_str()))
            .unwrap_or(base)
    }

    pub fn parse(key: &str) -> Result<Self> {
        let parts: Vec<_> = key.split('-').collect();
        if !(2..=3).contains(&parts.len()) {
            return Err(Error::InvalidConfig(format!("invalid platform key: {key}")));
        }
        let os = match parts[0] {
            "windows" => OperatingSystem::Windows,
            "linux" => OperatingSystem::Linux,
            "darwin" => OperatingSystem::MacOs,
            _ => {
                return Err(Error::InvalidConfig(format!(
                    "unsupported platform OS: {key}"
                )));
            }
        };
        let architecture = match parts[1] {
            "x86_64" => Architecture::X86_64,
            "aarch64" => Architecture::Aarch64,
            "i686" => Architecture::I686,
            "armv7" => Architecture::Armv7,
            _ => {
                return Err(Error::InvalidConfig(format!(
                    "unsupported platform architecture: {key}"
                )));
            }
        };
        let platform = Self::new(os, architecture);
        if let Some(format) = parts.get(2) {
            let format = match *format {
                "nsis" => PackageFormat::Nsis,
                "msi" => PackageFormat::Msi,
                "appimage" => PackageFormat::AppImage,
                "deb" => PackageFormat::Deb,
                "rpm" => PackageFormat::Rpm,
                "app" => PackageFormat::App,
                _ => {
                    return Err(Error::InvalidConfig(format!(
                        "unsupported updater package format: {key}"
                    )));
                }
            };
            platform.with_format(format)
        } else {
            Ok(platform)
        }
    }
    fn validate_artifact(self, name: &str) -> Result<()> {
        let matches = match self.format {
            Some(format) => format.matches_artifact(name),
            None => match self.os {
                OperatingSystem::Windows => {
                    PackageFormat::Nsis.matches_artifact(name)
                        || PackageFormat::Msi.matches_artifact(name)
                }
                OperatingSystem::Linux => PackageFormat::AppImage.matches_artifact(name),
                OperatingSystem::MacOs => PackageFormat::App.matches_artifact(name),
            },
        };
        if !matches {
            return Err(Error::InvalidConfig(format!(
                "artifact {name} does not match platform {}",
                self.key()
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestArtifact {
    pub signature: String,
    pub url: Url,
}

/// Compatible with existing ClipCat/GitCat `latest.json` files.
#[derive(Clone, Debug, Serialize)]
pub struct ReleaseManifest {
    version: Version,
    notes: String,
    platforms: BTreeMap<String, ManifestArtifact>,
}
impl ReleaseManifest {
    pub fn new(version: &str, notes: impl Into<String>) -> Result<Self> {
        Ok(Self {
            version: parse_version(version)?,
            notes: notes.into(),
            platforms: BTreeMap::new(),
        })
    }
    pub fn platforms(&self) -> &BTreeMap<String, ManifestArtifact> {
        &self.platforms
    }

    /// Load a native build job's manifest and validate its metadata before merging.
    /// Like `add`, this checks signature syntax, not remote artifact bytes.
    pub fn from_json(json: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct RawManifest {
            version: String,
            #[serde(default)]
            notes: String,
            platforms: BTreeMap<String, ManifestArtifact>,
        }
        let raw: RawManifest = serde_json::from_str(json)?;
        let mut manifest = Self::new(&raw.version, raw.notes)?;
        for (key, artifact) in raw.platforms {
            manifest.add(Platform::parse(&key)?, artifact.url, &artifact.signature)?;
        }
        if manifest.platforms.is_empty() {
            return Err(Error::InvalidConfig(
                "release manifest has no platforms".into(),
            ));
        }
        Ok(manifest)
    }

    /// Add an already signed remote artifact. Reject duplicate platform keys.
    pub fn add(&mut self, platform: Platform, url: Url, signature: &str) -> Result<()> {
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::InvalidConfig(
                "artifact URL must use HTTPS without credentials".into(),
            ));
        }
        let name = url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .unwrap_or_default();
        platform.validate_artifact(name)?;
        decode_signature(signature)?;
        let key = platform.key();
        if self.platforms.contains_key(&key) {
            return Err(Error::InvalidConfig(format!("duplicate platform: {key}")));
        }
        self.platforms.insert(
            key,
            ManifestArtifact {
                signature: signature.trim().into(),
                url,
            },
        );
        Ok(())
    }

    /// Read `<artifact>.sig`, verify the local bytes, then create its GitHub asset URL.
    pub fn add_file(
        &mut self,
        platform: Platform,
        repository: &Repository,
        tag: &str,
        artifact: &Path,
        public_key: &str,
    ) -> Result<()> {
        if parse_version(tag)? != self.version {
            return Err(Error::InvalidConfig(
                "release tag must match the manifest version".into(),
            ));
        }
        let name = artifact
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| Error::InvalidConfig("artifact filename must be UTF-8".into()))?;
        platform.validate_artifact(name)?;
        let mut signature_path = artifact.as_os_str().to_os_string();
        signature_path.push(".sig");
        let signature = std::fs::read_to_string(signature_path)?;
        verify_artifact(&std::fs::read(artifact)?, &signature, public_key)?;
        self.add(platform, repository.asset_url(tag, name), &signature)
    }

    /// Merge outputs of native build jobs. Version/notes and platform keys must agree.
    pub fn merge(&mut self, other: Self) -> Result<()> {
        if self.version != other.version || self.notes != other.notes {
            return Err(Error::InvalidConfig(
                "cannot merge manifests with different versions or notes".into(),
            ));
        }
        if other
            .platforms
            .keys()
            .any(|key| self.platforms.contains_key(key))
        {
            return Err(Error::InvalidConfig(
                "cannot merge duplicate platforms".into(),
            ));
        }
        self.platforms.extend(other.platforms);
        Ok(())
    }

    pub fn to_json(&self) -> Result<String> {
        if self.platforms.is_empty() {
            return Err(Error::InvalidConfig(
                "release manifest has no platforms".into(),
            ));
        }
        Ok(serde_json::to_string_pretty(self)?)
    }
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.to_json()?)?;
        Ok(())
    }
}

/// Builds native installers using the consumer's existing Tauri project.
/// Existing resources, dependency lists, NSIS templates/hooks and OS signing settings
/// remain in its configuration. Additional overrides use Tauri's normal merge rules.
pub struct InstallerBuilder {
    directory: PathBuf,
    cli: OsString,
    prefix: Vec<OsString>,
    formats: Vec<PackageFormat>,
    overrides: Value,
}
impl InstallerBuilder {
    pub fn new(project_directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: project_directory.into(),
            cli: "cargo".into(),
            prefix: vec!["tauri".into()],
            formats: Vec::new(),
            overrides: json!({}),
        }
    }

    /// For example `cli("node", ["node_modules/@tauri-apps/cli/tauri.js"])`.
    pub fn cli(
        mut self,
        program: impl Into<OsString>,
        prefix: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        self.cli = program.into();
        self.prefix = prefix.into_iter().map(Into::into).collect();
        self
    }
    pub fn formats(mut self, formats: impl IntoIterator<Item = PackageFormat>) -> Self {
        self.formats = formats.into_iter().collect();
        self
    }
    pub fn config(mut self, overrides: Value) -> Result<Self> {
        if !overrides.is_object()
            || overrides
                .get("bundle")
                .is_some_and(|value| !value.is_object())
        {
            return Err(Error::InvalidConfig(
                "config and bundle overrides must be JSON objects".into(),
            ));
        }
        self.overrides = overrides;
        Ok(self)
    }

    /// Creates a reviewable native build command; it does not execute it.
    pub fn plan(&self, host: OperatingSystem) -> Result<Command> {
        let formats = if self.formats.is_empty() {
            match host {
                OperatingSystem::Windows => vec![PackageFormat::Nsis],
                OperatingSystem::Linux => vec![
                    PackageFormat::AppImage,
                    PackageFormat::Deb,
                    PackageFormat::Rpm,
                ],
                OperatingSystem::MacOs => vec![PackageFormat::App, PackageFormat::Dmg],
            }
        } else {
            self.formats.clone()
        };
        if formats.iter().any(|format| format.os() != host) {
            return Err(Error::InvalidConfig(
                "build installers on their native operating system".into(),
            ));
        }
        let mut overrides = self.overrides.clone();
        if overrides.get("bundle").is_none() {
            overrides["bundle"] = json!({});
        }
        overrides["bundle"]["active"] = json!(true);
        overrides["bundle"]["createUpdaterArtifacts"] = json!(true);
        let mut command = self.command();
        command
            .arg("build")
            .arg("--bundles")
            .arg(
                formats
                    .iter()
                    .map(|f| f.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .arg("--config")
            .arg(serde_json::to_string(&overrides)?);
        Ok(command)
    }

    /// Inherits signing environment variables and streams CLI output to the caller.
    /// Run on a worker thread when called from an async application.
    pub fn build(&self) -> Result<()> {
        run(self.plan(OperatingSystem::current()?)?)
    }

    /// Explicitly sign deb/rpm (or any artifact not signed by the native bundler).
    /// Uses TAURI_SIGNING_PRIVATE_KEY and TAURI_SIGNING_PRIVATE_KEY_PASSWORD.
    /// Relative artifact paths are resolved against the configured project directory.
    pub fn sign(&self, artifact: impl AsRef<Path>) -> Result<()> {
        run(self.sign_plan(artifact)?)
    }

    /// Inspect the signing command without running it; the artifact must exist.
    pub fn sign_plan(&self, artifact: impl AsRef<Path>) -> Result<Command> {
        let artifact = artifact.as_ref();
        let artifact = if artifact.is_absolute() {
            artifact.to_path_buf()
        } else {
            self.directory.join(artifact)
        };
        let artifact = std::fs::canonicalize(artifact)?;
        let mut command = self.command();
        command.args(["signer", "sign"]).arg(artifact);
        Ok(command)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.cli);
        command.args(&self.prefix).current_dir(&self.directory);
        command
    }
}

fn run(mut command: Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(Error::Command(status.code()));
    }
    Ok(())
}

/// Config fragment for consumers setting up their updater plugin.
pub fn updater_config(repository: &Repository, public_key: &str) -> Result<Value> {
    decode_public_key(public_key)?;
    Ok(json!({
        "bundle": { "createUpdaterArtifacts": true },
        "plugins": { "updater": {
            "endpoints": [repository.latest_manifest_url()], "pubkey": public_key.trim(),
            "windows": { "installMode": "passive" }
        }}
    }))
}
