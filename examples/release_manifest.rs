//! `cargo run --example release_manifest -- 1.2.3 catninth/clipcat v1.2.3 public.key notes.md latest.json windows-x86_64 app-setup.exe`
//! Repeat PLATFORM ARTIFACT pairs for every package; run after all build jobs finish.
use catninth_updater::{
    Repository,
    installer::{Platform, ReleaseManifest},
};
use std::{env, fs, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() < 8 || !(args.len() - 6).is_multiple_of(2) {
        return Err("usage: release_manifest VERSION OWNER/REPO TAG PUBLIC_KEY_FILE NOTES_FILE OUTPUT [PLATFORM ARTIFACT]...".into());
    }
    let repository = Repository::parse(&args[1])?;
    let public_key = fs::read_to_string(&args[3])?;
    let mut manifest = ReleaseManifest::new(&args[0], fs::read_to_string(&args[4])?)?;
    for pair in args[6..].chunks_exact(2) {
        manifest.add_file(
            Platform::parse(&pair[0])?,
            &repository,
            &args[2],
            Path::new(&pair[1]),
            &public_key,
        )?;
    }
    manifest.write(&args[5])?;
    Ok(())
}
