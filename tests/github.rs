use catninth_updater::{
    Config, Error, Event, Operation, ReleaseSource, Repository, Updater, github::GitHubSource,
};
use serde_json::json;
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
    let url = format!("http://{}/api/v3/", listener.local_addr().unwrap())
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
fn response(tag: &str, body: Option<&str>) -> String {
    json!({"tag_name": tag, "body": body, "published_at": "2026-09-26T12:00:00Z", "draft": false, "prerelease": false}).to_string()
}
fn ok(body: String) -> (u16, String, String) {
    (200, String::new(), body)
}
fn source(server: &Server) -> GitHubSource {
    GitHubSource::new(
        Repository::parse("catninth/clipcat").unwrap(),
        Duration::from_secs(2),
    )
    .unwrap()
    .with_api_url(server.url.clone())
    .unwrap()
}

#[tokio::test]
async fn github_latest_uses_configured_repository_headers_and_markdown_body() {
    let mut server = serve(vec![ok(response(
        "v2.3.4",
        Some(" \n# Fixed\n\n- Capture race\n "),
    ))])
    .await;
    let release = source(&server)
        .with_token("test-token")
        .unwrap()
        .latest_release()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(release.version.to_string(), "2.3.4");
    assert_eq!(release.tag, "v2.3.4");
    assert_eq!(release.notes.as_deref(), Some("# Fixed\n\n- Capture race"));
    let request = server.requests.recv().await.unwrap();
    assert!(request.starts_with("GET /api/v3/repos/catninth/clipcat/releases/latest HTTP/1.1\r\n"));
    let request = request.to_ascii_lowercase();
    assert!(request.contains("user-agent: catninth-updater/"));
    assert!(request.contains("authorization: bearer test-token"));
    assert!(request.contains("accept: application/vnd.github+json"));
    assert!(request.contains("x-github-api-version: 2022-11-28"));
}

#[tokio::test]
async fn notes_use_exact_tag_then_semver_fallback_in_both_directions() {
    for (input, alternate) in [("2.3.4", "v2.3.4"), ("v2.3.4", "2.3.4")] {
        let mut server = serve(vec![
            (404, String::new(), "{}".into()),
            ok(response(alternate, Some("Old release"))),
        ])
        .await;
        assert_eq!(
            source(&server).patch_notes(input).await.unwrap().as_deref(),
            Some("Old release")
        );
        assert!(
            server
                .requests
                .recv()
                .await
                .unwrap()
                .contains(&format!("/releases/tags/{input} "))
        );
        assert!(
            server
                .requests
                .recv()
                .await
                .unwrap()
                .contains(&format!("/releases/tags/{alternate} "))
        );
    }
}

#[tokio::test]
async fn exact_non_semver_tag_is_encoded_as_one_segment() {
    let mut server = serve(vec![ok(response("release/one", Some("Tagged notes")))]).await;
    source(&server).patch_notes("release/one").await.unwrap();
    assert!(
        server
            .requests
            .recv()
            .await
            .unwrap()
            .contains("/releases/tags/release%2Fone ")
    );
}

#[tokio::test]
async fn drafts_prereleases_and_empty_notes_are_handled() {
    for field in ["draft", "prerelease"] {
        let mut body: serde_json::Value = serde_json::from_str(&response("nightly", None)).unwrap();
        body[field] = json!(true);
        let server = serve(vec![ok(body.to_string())]).await;
        assert!(source(&server).latest_release().await.unwrap().is_none());
    }
    let server = serve(vec![ok(response("2.0.0-beta.1", None))]).await;
    assert!(source(&server).latest_release().await.unwrap().is_none());
    let server = serve(vec![ok(response("2.0.0", Some(" \n ")))]).await;
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
}

#[tokio::test]
async fn invalid_json_tag_and_rate_limit_are_errors_not_up_to_date() {
    let server = serve(vec![ok("not json".into())]).await;
    assert!(matches!(
        source(&server).latest_release().await,
        Err(Error::Response(_))
    ));
    let server = serve(vec![ok(response("nightly", None))]).await;
    assert!(matches!(
        source(&server).latest_release().await,
        Err(Error::InvalidVersion(_))
    ));
    let server = serve(vec![(429, "Retry-After: 90\r\n".into(), "{}".into())]).await;
    assert!(
        matches!(source(&server).latest_release().await, Err(Error::Http { status: 429, retry_after: Some(value) }) if value == "90")
    );
}

#[tokio::test]
async fn failed_notes_request_emits_event_without_overwriting_update_state() {
    let server = serve(vec![(500, String::new(), "{}".into())]).await;
    let updater = Updater::builder(
        Config::new(Repository::parse("catninth/clipcat").unwrap(), "1.0.0", 5).unwrap(),
    )
    .source(Arc::new(source(&server)))
    .build()
    .unwrap();
    let before = updater.state();
    let mut events = updater.subscribe();
    assert!(updater.get_patch_notes("1.0.0").await.is_err());
    assert_eq!(updater.state(), before);
    assert!(matches!(
        events.recv().await.unwrap(),
        Event::Failed {
            operation: Operation::PatchNotes,
            ..
        }
    ));
}

#[tokio::test]
async fn configured_timeout_bounds_unresponsive_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let source = GitHubSource::new(
        Repository::parse("catninth/clipcat").unwrap(),
        Duration::from_millis(80),
    )
    .unwrap()
    .with_api_url(url)
    .unwrap();
    let task = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let result = source.latest_release().await;
    task.abort();
    assert!(matches!(result, Err(Error::Network(e)) if e.is_timeout()));
}

#[test]
fn credentials_and_insecure_remote_api_urls_are_rejected() {
    for url in [
        "http://example.com/",
        "https://user:pass@example.com/",
        "https://example.com/?token=secret",
        "file:///tmp/test",
    ] {
        let source =
            GitHubSource::new(Repository::parse("o/r").unwrap(), Duration::from_secs(1)).unwrap();
        assert!(source.with_api_url(url.parse().unwrap()).is_err(), "{url}");
    }
    assert!(
        GitHubSource::new(Repository::parse("o/r").unwrap(), Duration::from_secs(1))
            .unwrap()
            .with_token("\r\nheader: injected")
            .is_err()
    );
}
