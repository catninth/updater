# catninth-updater

A reusable Rust library for GitHub-based automatic updates and signed desktop
installer creation. It follows ClipCat's update lifecycle and incorporates
GitCat's requirements. Integrating either application is a separate task; this
repository builds and tests independently.

The core works without Tauri. The `tauri` feature connects the native installer
provided by `tauri-plugin-updater`, which both applications already use. Other
application frameworks can provide their own `InstallBackend` implementation
while sharing the same state machine, scheduling, and events.

## Integration

Add a local dependency to the consumer's `Cargo.toml`:

```toml
[dependencies]
catninth-updater = { path = "../updater", features = ["tauri"] }
```

Adjust the relative path to the consumer's `Cargo.toml`. Once pushed, the
`https://github.com/catninth/updater` repository can also be used as a Git
dependency, preferably pinned to a specific commit. The crate has not been
published to crates.io. Omit `features` for update checks without Tauri.

```rust,no_run
use catninth_updater::{Config, Event, Repository, Updater};
use std::time::Duration;

# async fn example() -> catninth_updater::Result<()> {
let config = Config::new(
    Repository::parse("catninth/clipcat")?,
    env!("CARGO_PKG_VERSION"), // The consumer's version, not the library's.
    360,                     // Check interval in minutes.
)?
.with_first_check_delay(Duration::from_secs(20))?
.with_request_timeout(Duration::from_secs(120))?;

let updater = Updater::builder(config)
    .on_event(|event| {
        if let Event::UpdateAvailable(release) = event {
            println!("New version available: {}", release.version);
        }
    })
    .build()?;

updater.fetch_version().await?;
let current = updater.get_installed_version();
let latest = updater.get_latest_version();
let notes = updater.get_patch_notes("v0.5.0").await?;

let polling = updater.start()?;
// Keep the polling handle in application state.
// On shutdown:
polling.stop().await?;
# Ok(())
# }
```

Async operations and `start()` require an active **Tokio runtime**. In Tauri, an
async command or `tauri::async_runtime::spawn` provides a suitable context.
Polling starts after the initial delay, then waits the configured number of
minutes after each completed check. Zero and overflowing intervals are rejected.
After a failure, the next polling cycle retries the check. If the updater is
busy, that scheduled check is skipped.

## API and state

| Requested operation | Rust API | Behavior |
| --- | --- | --- |
| `getInstalledVersion()` | `get_installed_version()` | Returns the running consumer's version without a network request. |
| `getLatestVersion()` | `get_latest_version()` | Returns the version cached by the last successful fetch; initially `None`. |
| `fetchVersion()` | `fetch_version().await` | Fetches the latest GitHub release and updates the cache and state. |
| `getPatchNotes(version)` | `get_patch_notes(version).await` | Fetches a release's Markdown description, including older releases. |
| Install | `install().await` | Downloads, verifies the signature, acquires the consumer gate, and installs natively. |
| Background checks | `start()` / `PollingHandle::stop()` | Controls a single polling loop that can be stopped. |
| Snapshot | `state()` | Returns serializable state for the UI. |
| Events | `subscribe()` / builder `on_event()` | Provides a Tokio broadcast receiver or callback. |

Patch notes come from the GitHub release **body**, not the repository's
`README.md`. Both `1.2.3` and `v1.2.3` work: the exact tag is tried first, followed
by the alternate form if GitHub returns 404. Custom tags can be requested by their
exact names. Missing releases and empty descriptions return `None`; network,
JSON, and HTTP failures return `Err`. GitHub may also return 404 for a private
repository that the caller cannot access.

Automatic checks follow stable releases. Drafts, prereleases, and SemVer
prerelease versions are excluded; the existing rolling `nightly` release is not
an automatic update channel. Version comparisons use SemVer precedence: updates
never downgrade, and build metadata alone does not make a version newer. The
latest release may be older than the installed version; it can be cached but
cannot be installed.

States: `Idle`, `Checking`, `Latest`, `Available`, `Downloading`, `Installing`,
`Installed`, and `Error`. `applied_version` identifies the version successfully
installed on disk; `current` remains the running process's version until restart.
The same version cannot be installed twice by the same updater instance.

Events: `StateChanged`, `UpdateAvailable`, `UpToDate`, `DownloadProgress`,
`Installed`, and `Failed`. `Failed.operation` distinguishes check, patch notes,
download, and install failures. Availability is announced once per version,
while state events are emitted on every check. A failed check or installation
preserves the previous release so installation can be retried. A failed request
from the notes panel does not overwrite an ongoing installation's state.

Progress includes cumulative bytes received and an optional percentage. An
unknown or zero download size produces `None` for the percentage. The broadcast
buffer stores 128 events; lagging consumers can recover with `state()`. Callbacks
run synchronously outside state locks: keep them short and do not block while
waiting for another updater operation. Unwinding panics are isolated; with
`panic = "abort"`, a panic exits the process.

A shared operation gate serializes checks and installation; conflicting calls
return `Error::Busy`. An operation that has started continues even if its caller
drops the awaiting future. This prevents a closed UI or cancelled command from
releasing the gate while a native installation is still running. Keep the runtime
alive until installation finishes. Dropping the polling handle stops future
checks; a check that has already started may still complete.

## Tauri adapter and consumer hooks

A compilable integration example is available in
[`examples/tauri_integration.rs`](examples/tauri_integration.rs).

1. Register `tauri_plugin_updater::Builder::new().build()` in the consumer.
2. Create a `TauriInstaller::new(app, repository, public_key)` instance and pass it
   to the updater builder's `.installer(Arc::new(installer))` method.
3. Set the consumer version from `app.package_info().version`. Use the same
   repository as the GitHub source. Set the adapter's own timeout with
   `.with_timeout(config.request_timeout())`.
4. Forward events to a consumer-defined Tauri event from `on_event`. The library
   does not register IPC commands or UI permissions.

The adapter reads `latest.json` from the selected release tag and checks the
manifest version. A release published in the meantime therefore cannot replace
the version the user selected. Tauri's minisign verification and automatic
package format selection remain in place. Installing an update requires a
signed package and the matching public key embedded in the application.

`InstallHooks` provides application-specific lifecycle hooks:

- `before_download`: performs an initial check, such as blocking installation
  while ClipCat is recording.
- `before_install`: checks again after downloading and atomically reserves the
  application for installation. The returned `InstallPermit` remains alive
  through installation and `after_install`; implement `Drop` to release the
  reservation on failure.
- `after_install`: on Linux/macOS, stops the engine, releases the single-instance
  lock, and optionally restarts. Automatic restart is disabled by default.
- `TauriInstaller::on_before_exit`: on Windows, Tauri exits when launching the
  installer. Stop the engine and save application state in this callback. The
  Tauri installer restarts the application by default. In this case,
  `after_install`, the `Installed` event, and Rust destructors are not guaranteed
  to run.

In ClipCat, recording startup and the final installation gate must use **the same
consumer operation lock**. Reading a `recording` boolean twice does not prevent
the race on its own. Engine shutdown, GitCat's `app_relaunch`, and
`tauri_plugin_single_instance::destroy` remain consumer responsibilities.

Private GitHub metadata is supported through `GitHubSource::with_token()`.
`with_api_url()` also accepts a GitHub Enterprise API endpoint. These settings
apply only to the metadata source: the bundled native adapter uses public
github.com release assets. Downloading private or Enterprise packages requires
a custom `InstallBackend`. The token is not automatically forwarded to package
URLs.

## Building installers and publishing releases

The `installer` module uses the Tauri 2 CLI to build native installers from the
consumer's existing project. Install the CLI and the target OS's build
dependencies first. The consumer configuration supplies icons, resources, Linux
dependencies, NSIS templates/hooks, and OS code-signing/notarization settings.

```rust,no_run
use catninth_updater::installer::{InstallerBuilder, OperatingSystem};

# fn example() -> catninth_updater::Result<()> {
let installer = InstallerBuilder::new("../consumer");
let command = installer.plan(OperatingSystem::current()?)?; // Inspect the command.
installer.build()?;                                       // Run the native build.
# Ok(())
# }
```

The default CLI is `cargo tauri`. To use the Node CLI, configure
`.cli("node", ["node_modules/@tauri-apps/cli/tauri.js"])`.
`.config(serde_json::Value)` passes overrides through Tauri's normal configuration
merge behavior. The builder enforces `bundle.active` and
`createUpdaterArtifacts`. `.formats(...)` overrides the native format list.
Arguments are passed directly to the process without shell concatenation.
Build and signing operations block; run them on a worker thread in async
applications.

| OS | Default installers | Auto-update artifact |
| --- | --- | --- |
| Windows | NSIS; optionally MSI | `.exe` / `.msi` + `.sig` |
| Linux | AppImage, deb, rpm | `.AppImage`, `.deb`, `.rpm` + `.sig` |
| macOS | `.app`, DMG | `.app.tar.gz` + `.sig`; DMG is for initial installation |

Build each package on its native OS. Separate macOS builds can target Intel and
Apple Silicon. Updater signatures do not replace Apple notarization or Windows
Authenticode signatures.

Release steps:

1. Set `TAURI_SIGNING_PRIVATE_KEY` and, if needed,
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in the build environment. Preserve the
   consumer's existing key so installed versions can verify the next release.
2. Run `InstallerBuilder::build()` or the `package` example. If an artifact has no
   `.sig` file, especially deb/rpm packages, use `InstallerBuilder::sign(artifact)`.
   Relative artifact paths are resolved against the configured project directory.
3. `ReleaseManifest::add_file()` verifies the local package signature against the
   public key and adds its GitHub download URL. The tag must match the manifest
   version.
4. `write("latest.json")` writes a manifest compatible with the existing release
   pipelines. Combine separate native build outputs with `from_json()` and
   `merge()`. Different versions or notes and duplicate platforms are rejected.
   `add()` and `from_json()` validate remote entry structure and signature syntax;
   `add_file()` or the runtime installer verifies the actual artifact bytes.
5. The consumer's release workflow uploads packages, `.sig` files, and
   `latest.json` to the appropriate GitHub release. The library does not publish
   releases automatically.

Linux platform keys are `linux-x86_64` for the AppImage fallback, plus separate
`linux-x86_64-appimage`, `linux-x86_64-deb`, and `linux-x86_64-rpm` entries.
macOS uses `darwin-aarch64` and `darwin-x86_64`. Windows uses `windows-x86_64`, with
optional `-nsis` or `-msi` suffixes. `Platform` generates these keys and validates
the artifact format. The consumer must build and upload the package for each
architecture it supports.

Runnable examples:

```sh
cargo run --example check -- catninth/clipcat 0.5.0
cargo run --example package -- /path/to/consumer
cargo run --example release_manifest -- 1.2.3 catninth/clipcat v1.2.3 public.key notes.md latest.json windows-x86_64 app-setup.exe linux-x86_64 app.AppImage linux-x86_64-deb app.deb darwin-aarch64 App.app.tar.gz
```

## Tests and project inventory

```sh
cargo fmt --all -- --check
cargo test --locked --no-default-features --all-targets
cargo test --locked --all-features --all-targets
cargo test --locked --all-features --doc
cargo clippy --locked --all-features --all-targets -- -D warnings
```

Tests use a local HTTP server, virtual time, and a simulated installer. They need
no GitHub token, live release, or actual application replacement. A real minisign
test vector checks both authentic and modified bytes. CI runs the core and the
Tauri feature on Windows, Linux, and macOS. Actual installer launch, elevation
prompts, restart, and macOS notarization require native smoke tests as part of
consumer integration.

The requirements inventory and future migration boundaries are documented in
[`docs/project-inventory.md`](docs/project-inventory.md).

API references: [GitHub Releases REST API](https://docs.github.com/en/rest/releases/releases),
[Tauri updater](https://v2.tauri.app/plugin/updater/),
[Tauri CLI](https://v2.tauri.app/reference/cli/).
