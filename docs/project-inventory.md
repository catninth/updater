# Project inventory and design decisions

## Projects inspected

The workspace contains `clipcat`, `gitcat`, `cutcat`, and `updater`.
As requested, `cutcat` was excluded. Initially, `updater` contained only a README.

### ClipCat

- `src-tauri/src/updater.rs`: a 20-second startup delay, checks every six hours,
  a state machine, one notification per version, retryable updates, and download
  progress.
- The Tauri plugin downloads signed packages and performs native installation.
- Installation is blocked while recording or saving. A second check after
  downloading must share an operation gate with recording startup.
- On Windows, the engine stops before exit. On Linux, the engine stops and the
  single-instance lock is released before restart.
- `tauri.windows.conf.json`, `installer-hooks.nsh`: per-user NSIS installation,
  OBS resources, preserved autostart settings during updates, and optional
  application data removal.
- `tauri.linux.conf.json`: AppImage/deb/rpm packages and OBS/ffmpeg dependencies.
- `.github/workflows/release.yml`: signatures, separate Linux package format keys,
  `latest.json`, stable tags, and a separate rolling nightly release.

### GitCat

- `apps/desktop/src/lib/updates.ts`: a UI-side Tauri updater wrapper with a startup
  check, release notes, download progress, and retry after failed operations.
- `apps/desktop/src-tauri/src/lib.rs`: `app_relaunch` releases the single-instance
  lock before restart.
- `apps/desktop/src-tauri/nsis/installer.nsi`, `nsis/hooks.nsh`: a custom NSIS
  interface and Explorer context menu; updates preserve the user's selection.
- `.github/workflows/release.yml`: Windows NSIS, Linux AppImage/deb/rpm, signed
  update manifests, and a separate nightly channel.
- The current updater endpoint uses `RisDN/gitcat`, while ClipCat uses
  `catninth/clipcat`. The framework does not assume a shared owner or repository.

## Responsibilities moved into the library

Configurable repository, consumer version, check interval in minutes, startup
delay, HTTP timeout, stable release fetching and caching, release-body retrieval,
state machine, one-time availability notifications, callbacks/events, progress,
mutually exclusive operations, retries, and installation hooks. The optional
Tauri adapter preserves the existing native installer and signature verification.

The installer module builds from the existing Tauri configuration, preserving
OBS resources, NSIS hooks, the Explorer menu, and Linux dependencies. It provides
signing commands, artifact verification, platform keys, and manifest generation.
Application-specific NSIS templates remain in their consumer projects.

## Consumer responsibilities

UI, localization, toasts, logging, recording and Git operation lifecycles, sidecar
shutdown, single-instance locks, restart, icons and resources, NSIS choices,
OS code signing, and release permissions. The framework accepts these through
hooks and Tauri configuration instead of hardcoding application-specific behavior.

## Future migration

1. ClipCat: connect the existing updater state and Tauri commands to the library
   API. Preserve the operations/installing/quitting protection with a concrete
   `InstallPermit` implementation. Trigger toasts from `UpdateAvailable`.
2. GitCat: move check/install orchestration into Rust and have the UI hook consume
   serialized library state. Preserve `app_relaunch` behavior.
3. Both release pipelines: preserve the existing signing keys and asset formats.
   Manifest generation can move to the `installer` module.
4. Test an actual version upgrade for every supported native package type,
   checking that installer preferences and user data are preserved.

Consumer migration is outside this task; both consumer repositories remain
unchanged.
