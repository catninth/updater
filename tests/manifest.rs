use catninth_updater::{
    Config, Error, Phase, ReleaseSource, Repository, Updater, manifest::GitHubManifestSource,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    task::JoinHandle,
};

struct Server {
    url: url::Url,
    requests: mpsc::UnboundedReceiver<String>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(responses: Vec<(u16, String, String)>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mirror/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let (sender, requests) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        for (status, extra_headers, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            sender.send(String::from_utf8(request).unwrap()).unwrap();
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    Server {
        url,
        requests,
        task,
    }
}
fn repository() -> Repository {
    Repository::parse("catninth/clipcat").unwrap()
}
fn manifest(tag: &str, notes: Option<&str>) -> Value {
    json!({
        "version": tag.strip_prefix('v').unwrap_or(tag),
        "notes": notes,
        "pub_date": "2026-10-13T12:00:00Z",
        "platforms": {
            "windows-x86_64": {
                "url": repository().asset_url(tag, "ClipCat-setup.exe"),
                "signature": "verified-by-installer"
            },
            "linux-x86_64": {
                "url": repository().asset_url(tag, "ClipCat.AppImage"),
                "signature": "verified-by-installer"
            }
        }
    })
}
fn ok(body: Value) -> (u16, String, String) {
    (200, String::new(), body.to_string())
}
fn source(server: &Server) -> GitHubManifestSource {
    GitHubManifestSource::new(repository(), Duration::from_secs(2))
        .unwrap()
        .with_base_url(server.url.clone())
        .unwrap()
}

#[tokio::test]
async fn public_manifest_check_populates_update_state_without_api_or_credentials() {
    let mut server = serve(vec![ok(manifest(
        "v0.8.1",
        Some(" \n# Fixed\n\n- License link\n "),
    ))])
    .await;
    let updater = Updater::builder(Config::new(repository(), "0.8.0", 360).unwrap())
        .source(Arc::new(source(&server)))
        .build()
        .unwrap();
    let release = updater.fetch_version().await.unwrap().unwrap();
    assert_eq!(updater.state().phase, Phase::Available);
    assert_eq!(release.version.to_string(), "0.8.1");
    assert_eq!(release.tag, "v0.8.1");
    assert_eq!(release.notes.as_deref(), Some("# Fixed\n\n- License link"));
    assert_eq!(
        release.published_at.as_deref(),
        Some("2026-10-13T12:00:00Z")
    );
    let request = server.requests.recv().await.unwrap().to_ascii_lowercase();
    assert!(request.starts_with(
        "get /mirror/catninth/clipcat/releases/latest/download/latest.json http/1.1\r\n"
    ));
    assert!(request.contains("user-agent: catninth-updater/"));
    assert!(request.contains("accept: application/json"));
    assert!(!request.contains("authorization:"));
    assert!(!request.contains("x-github-api-version:"));
    assert!(!request.contains("/repos/"));
    assert!(server.requests.recv().await.is_none());
}

#[tokio::test]
async fn exact_supported_tag_survives_prefix_and_build_metadata() {
    for tag in ["v0.8.1", "0.8.1", "v0.8.1+build.7", "0.8.1+build.7"] {
        let server = serve(vec![ok(manifest(tag, None))]).await;
        let release = source(&server).latest_release().await.unwrap().unwrap();
        assert_eq!(release.tag, tag);
        assert_eq!(release.version.to_string(), tag.trim_start_matches('v'));
        assert!(release.notes.is_none());
    }
}

#[tokio::test]
async fn redirect_to_opaque_asset_path_preserves_manifest_tag() {
    let mut server = serve(vec![
        (302, "Location: /opaque/asset-id\r\n".into(), String::new()),
        ok(manifest("0.8.1", Some("notes"))),
    ])
    .await;
    let release = source(&server).latest_release().await.unwrap().unwrap();
    assert_eq!(release.tag, "0.8.1");
    server.requests.recv().await.unwrap();
    let redirected = server.requests.recv().await.unwrap();
    assert!(redirected.starts_with("GET /opaque/asset-id HTTP/1.1"));
    assert!(!redirected.to_ascii_lowercase().contains("authorization:"));
}

#[tokio::test]
async fn notes_use_tag_pinned_manifest_and_only_try_alternate_after_not_found() {
    for (input, tag) in [("0.8.1", "v0.8.1"), ("v0.8.1", "0.8.1")] {
        let mut server = serve(vec![
            (404, String::new(), "{}".into()),
            ok(manifest(tag, Some(" Previous notes \n"))),
        ])
        .await;
        assert_eq!(
            source(&server).patch_notes(input).await.unwrap().as_deref(),
            Some("Previous notes")
        );
        for expected in [input, tag] {
            let request = server.requests.recv().await.unwrap();
            assert!(request.contains(&format!("/releases/download/{expected}/latest.json ")));
        }
    }
    let mut server = serve(vec![ok(manifest("v0.8.1", Some("exact")))]).await;
    assert_eq!(
        source(&server)
            .patch_notes("v0.8.1")
            .await
            .unwrap()
            .as_deref(),
        Some("exact")
    );
    server.requests.recv().await.unwrap();
    assert!(server.requests.recv().await.is_none());
}

#[tokio::test]
async fn notes_reject_mismatched_release_or_version() {
    for tag in ["0.8.1", "v0.8.2"] {
        let server = serve(vec![ok(manifest(tag, Some("wrong release")))]).await;
        assert!(matches!(
            source(&server).patch_notes("v0.8.1").await,
            Err(Error::InvalidConfig(_))
        ));
    }
}

#[tokio::test]
async fn prereleases_missing_metadata_empty_notes_and_not_found_are_handled() {
    let server = serve(vec![ok(manifest("v0.8.2-beta.1", None))]).await;
    assert!(source(&server).latest_release().await.unwrap().is_none());
    let mut body = manifest("v0.8.1", Some(" \n "));
    body.as_object_mut().unwrap().remove("pub_date");
    let server = serve(vec![ok(body)]).await;
    let release = source(&server).latest_release().await.unwrap().unwrap();
    assert!(release.notes.is_none());
    assert!(release.published_at.is_none());
    let mut body = manifest("v0.8.1", None);
    body.as_object_mut().unwrap().remove("notes");
    let server = serve(vec![ok(body)]).await;
    assert!(
        source(&server)
            .latest_release()
            .await
            .unwrap()
            .unwrap()
            .notes
            .is_none()
    );
    let server = serve(vec![(404, String::new(), "{}".into())]).await;
    assert!(source(&server).latest_release().await.unwrap().is_none());
    let server = serve(vec![
        (404, String::new(), "{}".into()),
        (404, String::new(), "{}".into()),
    ])
    .await;
    assert!(
        source(&server)
            .patch_notes("0.8.1")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn malformed_manifests_remain_errors() {
    for body in ["not json", "{}"] {
        let server = serve(vec![(200, String::new(), body.into())]).await;
        assert!(matches!(
            source(&server).latest_release().await,
            Err(Error::Response(_))
        ));
    }
    let mut body = manifest("v0.8.1", None);
    body["version"] = json!("nightly");
    let server = serve(vec![ok(body)]).await;
    assert!(matches!(
        source(&server).latest_release().await,
        Err(Error::InvalidVersion(_))
    ));
    let mut body = manifest("v0.8.1", None);
    body["platforms"] = json!({});
    let server = serve(vec![ok(body)]).await;
    assert!(matches!(
        source(&server).latest_release().await,
        Err(Error::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn every_artifact_must_share_valid_repository_tag_and_https_url() {
    for url in [
        "https://example.com/catninth/clipcat/releases/download/v0.8.1/app.exe",
        "https://github.com.evil.example/catninth/clipcat/releases/download/v0.8.1/app.exe",
        "https://github.com/catninth/other/releases/download/v0.8.1/app.exe",
        "https://github.com/other/clipcat/releases/download/v0.8.1/app.exe",
        "https://github.com/catninth/clipcat/releases/download/v0.8.2/app.exe",
        "https://github.com/catninth/clipcat/releases/download/0.8.1/app.exe",
        "https://github.com/catninth/clipcat/releases/download/v0.8.10/app.exe",
        "http://github.com/catninth/clipcat/releases/download/v0.8.1/app.exe",
        "https://user:pass@github.com/catninth/clipcat/releases/download/v0.8.1/app.exe",
        "https://github.com/catninth/clipcat/releases/download/v0.8.1/app.exe?token=secret",
        "https://github.com/catninth/clipcat/releases/download/v0.8.1/app.exe#fragment",
        "https://github.com/catninth/clipcat/releases/download/v0.8.1/",
        "https://github.com/catninth/clipcat/releases/download/v0.8.1/nested/app.exe",
    ] {
        let mut body = manifest("v0.8.1", None);
        body["platforms"]["linux-x86_64"]["url"] = json!(url);
        let server = serve(vec![ok(body)]).await;
        assert!(
            matches!(
                source(&server).latest_release().await,
                Err(Error::InvalidConfig(_))
            ),
            "{url}"
        );
    }
}

#[tokio::test]
async fn http_errors_do_not_fallback_or_report_up_to_date() {
    for status in [403, 429, 500] {
        let mut server = serve(vec![(status, "Retry-After: 90\r\n".into(), "{}".into())]).await;
        assert!(matches!(
            source(&server).patch_notes("0.8.1").await,
            Err(Error::Http { status: actual, retry_after: Some(value) })
                if actual == status && value == "90"
        ));
        server.requests.recv().await.unwrap();
        assert!(server.requests.recv().await.is_none());
    }
    let server = serve(vec![(403, String::new(), "{}".into())]).await;
    let updater = Updater::builder(Config::new(repository(), "0.8.0", 360).unwrap())
        .source(Arc::new(source(&server)))
        .build()
        .unwrap();
    assert!(updater.fetch_version().await.is_err());
    assert_eq!(updater.state().phase, Phase::Error);
}

#[tokio::test]
async fn configured_timeout_bounds_unresponsive_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let source = GitHubManifestSource::new(repository(), Duration::from_millis(80))
        .unwrap()
        .with_base_url(url)
        .unwrap();
    let task = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let result = source.latest_release().await;
    task.abort();
    assert!(matches!(result, Err(Error::Network(error)) if error.is_timeout()));
}

#[tokio::test]
async fn unsafe_redirects_are_rejected_before_following() {
    for location in [
        "http://example.com/manifest.json",
        "https://user:pass@example.com/manifest.json",
    ] {
        let mut server = serve(vec![(
            302,
            format!("Location: {location}\r\n"),
            String::new(),
        )])
        .await;
        assert!(matches!(
            source(&server).latest_release().await,
            Err(Error::Network(_))
        ));
        server.requests.recv().await.unwrap();
        assert!(server.requests.recv().await.is_none());
    }
}

#[test]
fn invalid_timeouts_and_unsafe_base_urls_are_rejected() {
    for timeout in [Duration::ZERO, Duration::MAX] {
        assert!(GitHubManifestSource::new(repository(), timeout).is_err());
    }
    for url in [
        "http://example.com/",
        "https://user:pass@example.com/",
        "https://example.com/?token=secret",
        "https://example.com/#fragment",
        "file:///tmp/test",
    ] {
        assert!(
            GitHubManifestSource::new(repository(), Duration::from_secs(1))
                .unwrap()
                .with_base_url(url.parse().unwrap())
                .is_err(),
            "{url}"
        );
    }
}

#[tokio::test]
async fn blank_notes_tag_is_rejected_without_requesting() {
    let source = GitHubManifestSource::new(repository(), Duration::from_secs(1)).unwrap();
    assert!(matches!(
        source.patch_notes(" \n ").await,
        Err(Error::InvalidConfig(_))
    ));
}
