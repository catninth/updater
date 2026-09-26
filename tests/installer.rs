use base64::{Engine, engine::general_purpose::STANDARD};
use catninth_updater::{Repository, installer::*};
use serde_json::json;

// Public test vector from minisign-verify 0.2.5 (MIT), author Frank Denis:
// https://github.com/jedisct1/rust-minisign-verify/blob/0.2.5/src/lib.rs
// Signs exactly b"test". No private key or consumer signing material is used.
fn public_key() -> String {
    STANDARD.encode("untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3")
}
fn signature() -> String {
    STANDARD.encode("untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1633700835\tfile:test\tprehashed\nwLMDjy9FLAuxZ3q4NlEvkgtyhrr0gtTu6KC4KBJdITbbOeAi1zBIYo0v4iTgt8jJpIidRJnp94ABQkJAgAooBQ==")
}
fn windows() -> Platform {
    Platform::new(OperatingSystem::Windows, Architecture::X86_64)
}

#[test]
fn authentic_signature_passes_and_tampered_bytes_or_signatures_fail() {
    verify_artifact(b"test", &signature(), &public_key()).unwrap();
    assert!(verify_artifact(b"Test", &signature(), &public_key()).is_err());
    assert!(verify_artifact(b"test", "not-base64", &public_key()).is_err());
    assert!(verify_artifact(b"test", &signature(), "wrong-key").is_err());
    let bad_signature = signature().replace('R', "S");
    assert!(verify_artifact(b"test", &bad_signature, &public_key()).is_err());
}

#[test]
fn package_config_preserves_consumer_hooks_and_enforces_signed_update_artifacts() {
    let builder = InstallerBuilder::new("consumer project").config(json!({
        "bundle": {"createUpdaterArtifacts": false, "windows": {"nsis": {"installerHooks": "hooks.nsh", "template": "installer.nsi"}}, "resources": ["obs/"]}
    })).unwrap();
    let plan = builder.plan(OperatingSystem::Windows).unwrap();
    let args: Vec<_> = plan
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(&args[..4], ["tauri", "build", "--bundles", "nsis"]);
    let config: serde_json::Value = serde_json::from_str(args.last().unwrap()).unwrap();
    assert_eq!(config["bundle"]["createUpdaterArtifacts"], true);
    assert_eq!(
        config["bundle"]["windows"]["nsis"]["installerHooks"],
        "hooks.nsh"
    );
    assert_eq!(config["bundle"]["resources"][0], "obs/");
    assert_eq!(
        plan.get_current_dir().unwrap(),
        std::path::Path::new("consumer project")
    );
}

#[test]
fn native_defaults_and_cross_os_format_rejection() {
    for (os, expected) in [
        (OperatingSystem::Windows, "nsis"),
        (OperatingSystem::Linux, "appimage,deb,rpm"),
        (OperatingSystem::MacOs, "app,dmg"),
    ] {
        let plan = InstallerBuilder::new(".").plan(os).unwrap();
        assert!(plan.get_args().any(|arg| arg == expected));
    }
    assert!(
        InstallerBuilder::new(".")
            .formats([PackageFormat::Nsis])
            .plan(OperatingSystem::Linux)
            .is_err()
    );
    assert!(
        InstallerBuilder::new(".")
            .config(json!({"bundle": false}))
            .is_err()
    );
    assert!(InstallerBuilder::new(".").config(json!([])).is_err());
}

#[test]
fn manifest_includes_all_os_targets_and_preserves_linux_install_type() {
    let repository = Repository::parse("catninth/clipcat").unwrap();
    let mut manifest = ReleaseManifest::new("v2.0.0", "# Notes").unwrap();
    for (platform, artifact) in [
        (windows(), "app-setup.exe"),
        (
            Platform::new(OperatingSystem::Linux, Architecture::X86_64),
            "app.AppImage",
        ),
        (
            Platform::new(OperatingSystem::Linux, Architecture::X86_64)
                .with_format(PackageFormat::Deb)
                .unwrap(),
            "app.deb",
        ),
        (
            Platform::new(OperatingSystem::Linux, Architecture::X86_64)
                .with_format(PackageFormat::Rpm)
                .unwrap(),
            "app.rpm",
        ),
        (
            Platform::new(OperatingSystem::MacOs, Architecture::Aarch64),
            "App.app.tar.gz",
        ),
        (
            Platform::new(OperatingSystem::MacOs, Architecture::X86_64),
            "App-intel.app.tar.gz",
        ),
    ] {
        manifest
            .add(
                platform,
                repository.asset_url("v2.0.0", artifact),
                &signature(),
            )
            .unwrap();
    }
    let json: serde_json::Value = serde_json::from_str(&manifest.to_json().unwrap()).unwrap();
    assert_eq!(json["version"], "2.0.0");
    assert_eq!(json["platforms"].as_object().unwrap().len(), 6);
    assert!(
        json["platforms"]["linux-x86_64-deb"]["url"]
            .as_str()
            .unwrap()
            .ends_with("app.deb")
    );
    assert!(
        json["platforms"]["darwin-aarch64"]["url"]
            .as_str()
            .unwrap()
            .ends_with("App.app.tar.gz")
    );
    #[cfg(feature = "tauri")]
    {
        let _: tauri_plugin_updater::RemoteRelease = serde_json::from_value(json).unwrap();
    }
}

#[test]
fn local_artifact_is_verified_before_manifest_is_written() {
    let temp = tempfile::tempdir().unwrap();
    let artifact = temp.path().join("App setup.exe");
    std::fs::write(&artifact, b"test").unwrap();
    std::fs::write(temp.path().join("App setup.exe.sig"), signature()).unwrap();
    let mut manifest = ReleaseManifest::new("2.0.0", "Notes").unwrap();
    let repo = Repository::parse("catninth/clipcat").unwrap();
    manifest
        .add_file(windows(), &repo, "v2.0.0", &artifact, &public_key())
        .unwrap();
    let output = temp.path().join("latest.json");
    manifest.write(&output).unwrap();
    assert!(
        std::fs::read_to_string(output)
            .unwrap()
            .contains("App%20setup.exe")
    );
    std::fs::write(&artifact, b"tampered").unwrap();
    let mut manifest = ReleaseManifest::new("2.0.0", "Notes").unwrap();
    assert!(
        manifest
            .add_file(windows(), &repo, "v2.0.0", &artifact, &public_key())
            .is_err()
    );
    assert!(manifest.platforms().is_empty());
}

#[test]
fn rejects_missing_signature_wrong_tag_wrong_platform_unsigned_and_duplicate_entries() {
    let temp = tempfile::tempdir().unwrap();
    let artifact = temp.path().join("app.exe");
    std::fs::write(&artifact, b"test").unwrap();
    let repo = Repository::parse("catninth/clipcat").unwrap();
    let mut manifest = ReleaseManifest::new("2.0.0", "").unwrap();
    assert!(manifest.to_json().is_err());
    assert!(
        manifest
            .add_file(windows(), &repo, "v2.0.0", &artifact, &public_key())
            .is_err()
    );
    assert!(
        manifest
            .add_file(windows(), &repo, "v3.0.0", &artifact, &public_key())
            .is_err()
    );
    assert!(
        manifest
            .add(
                windows(),
                "http://example.com/app.exe".parse().unwrap(),
                &signature()
            )
            .is_err()
    );
    assert!(
        manifest
            .add(
                windows(),
                "https://example.com/app.exe".parse().unwrap(),
                ""
            )
            .is_err()
    );
    assert!(
        manifest
            .add(
                windows(),
                "https://example.com/app.deb".parse().unwrap(),
                &signature()
            )
            .is_err()
    );
    manifest
        .add(
            windows(),
            "https://example.com/app.exe".parse().unwrap(),
            &signature(),
        )
        .unwrap();
    assert!(
        manifest
            .add(
                windows(),
                "https://example.com/other.exe".parse().unwrap(),
                &signature()
            )
            .is_err()
    );
    assert!(
        Platform::new(OperatingSystem::MacOs, Architecture::Aarch64)
            .with_format(PackageFormat::Dmg)
            .is_err()
    );
    assert!(windows().with_format(PackageFormat::Rpm).is_err());
}

#[test]
fn merging_is_atomic_on_conflicts() {
    let mut first = ReleaseManifest::new("1.0.0", "Notes").unwrap();
    first
        .add(
            windows(),
            "https://example.com/app.exe".parse().unwrap(),
            &signature(),
        )
        .unwrap();
    assert!(first.merge(first.clone()).is_err());
    assert_eq!(first.platforms().len(), 1);
    assert!(
        first
            .merge(ReleaseManifest::new("2.0.0", "Notes").unwrap())
            .is_err()
    );
    let mut second = ReleaseManifest::new("1.0.0", "Notes").unwrap();
    second
        .add(
            Platform::new(OperatingSystem::Linux, Architecture::X86_64),
            "https://example.com/app.AppImage".parse().unwrap(),
            &signature(),
        )
        .unwrap();
    first.merge(second).unwrap();
    assert_eq!(first.platforms().len(), 2);
}

#[test]
fn generated_updater_config_uses_consumer_repository_and_key() {
    let repo = Repository::parse("another/product").unwrap();
    let config = updater_config(&repo, &public_key()).unwrap();
    assert_eq!(
        config["plugins"]["updater"]["endpoints"][0],
        "https://github.com/another/product/releases/latest/download/latest.json"
    );
    assert_eq!(config["plugins"]["updater"]["pubkey"], public_key());
    assert!(updater_config(&repo, "not-a-key").is_err());
}

#[test]
fn imported_build_manifests_are_validated_and_round_trip() {
    let mut manifest = ReleaseManifest::new("1.0.0", "Notes").unwrap();
    manifest
        .add(
            windows(),
            "https://example.com/app.exe".parse().unwrap(),
            &signature(),
        )
        .unwrap();
    let json = manifest.to_json().unwrap();
    assert_eq!(
        ReleaseManifest::from_json(&json)
            .unwrap()
            .to_json()
            .unwrap(),
        json
    );
    let bad = json.replace("windows-x86_64", "linux-x86_64-deb");
    assert!(ReleaseManifest::from_json(&bad).is_err());
    assert!(ReleaseManifest::from_json(r#"{"version":"1.0.0","platforms":{}}"#).is_err());
    assert!(ReleaseManifest::from_json(&json.replace("https://", "http://")).is_err());
    for key in [
        "linux-x86_64",
        "linux-x86_64-appimage",
        "linux-aarch64-deb",
        "linux-armv7-rpm",
        "windows-i686-nsis",
        "windows-x86_64-msi",
        "darwin-aarch64-app",
    ] {
        assert_eq!(Platform::parse(key).unwrap().key(), key);
    }
    for key in [
        "macos-x86_64",
        "linux-mips",
        "darwin-aarch64-dmg",
        "windows-x86_64-deb",
        "windows-x86_64-nsis-extra",
    ] {
        assert!(Platform::parse(key).is_err(), "{key}");
    }
}

#[test]
fn signing_resolves_artifact_relative_to_consumer_and_preserves_spaces() {
    let temp = tempfile::tempdir().unwrap();
    let artifact = temp.path().join("App setup.exe");
    std::fs::write(&artifact, b"test").unwrap();
    let plan = InstallerBuilder::new(temp.path())
        .sign_plan("App setup.exe")
        .unwrap();
    let args: Vec<_> = plan.get_args().collect();
    assert_eq!(args.len(), 4);
    assert_eq!(args[1], "signer");
    assert_eq!(args[2], "sign");
    assert_eq!(
        std::path::Path::new(args[3]),
        artifact.canonicalize().unwrap()
    );
}
