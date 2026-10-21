//! TDD battery for the content-bundle manifest and loader (ADR-0045): a manifest names the
//! `{engine, content, schema}` version triple, the entry-point Rhai script and every asset by its
//! blake3 content id; the loader returns the assets only when every id matches. Bundles are
//! user-generated content, so a wrong hash, a missing asset or a path that escapes the bundle
//! directory is an error. Written before the implementation.

use std::collections::HashMap;
use std::path::Path;

use protocol::bundle::{BundleError, load_bundle, load_bundle_dir};
use protocol::{VersionTriple, content_id};

const SCRIPT: &str = "fn update(a) { a }";

fn manifest(entry: &str, assets: &[(&str, String)]) -> String {
    let assets: Vec<String> = assets
        .iter()
        .map(|(path, id)| format!(r#"{{ "path": "{path}", "id": "{id}" }}"#))
        .collect();
    format!(
        r#"{{ "version": {{ "engine": 1, "content": 2, "schema": 3 }}, "entry": "{entry}", "assets": [{}] }}"#,
        assets.join(", ")
    )
}

fn store(files: &[(&str, &[u8])]) -> HashMap<String, Vec<u8>> {
    files
        .iter()
        .map(|(p, b)| (p.to_string(), b.to_vec()))
        .collect()
}

#[test]
fn a_matching_bundle_loads() {
    let files = store(&[("game.rhai", SCRIPT.as_bytes()), ("logo.png", b"\x89PNG")]);
    let json = manifest(
        "game.rhai",
        &[
            ("game.rhai", content_id(SCRIPT.as_bytes()).to_hex()),
            ("logo.png", content_id(b"\x89PNG").to_hex()),
        ],
    );
    let bundle = load_bundle(&json, |p| files.get(p).cloned()).unwrap();
    assert_eq!(
        bundle.version,
        VersionTriple {
            engine: 1,
            content: 2,
            schema: 3
        }
    );
    assert_eq!(bundle.entry, "game.rhai");
    assert_eq!(bundle.entry_source, SCRIPT);
    assert_eq!(bundle.assets.len(), 2);
    assert_eq!(bundle.assets["logo.png"], b"\x89PNG");
}

#[test]
fn a_hash_mismatch_is_rejected() {
    let files = store(&[("game.rhai", b"fn update(a) { a.vx = 1e9; a }")]);
    let json = manifest(
        "game.rhai",
        &[("game.rhai", content_id(SCRIPT.as_bytes()).to_hex())],
    );
    let err = load_bundle(&json, |p| files.get(p).cloned()).unwrap_err();
    assert!(
        matches!(err, BundleError::HashMismatch { ref path } if path == "game.rhai"),
        "{err:?}"
    );
}

#[test]
fn a_missing_asset_is_rejected() {
    let files = store(&[]);
    let json = manifest(
        "game.rhai",
        &[("game.rhai", content_id(SCRIPT.as_bytes()).to_hex())],
    );
    let err = load_bundle(&json, |p| files.get(p).cloned()).unwrap_err();
    assert!(
        matches!(err, BundleError::MissingAsset(ref p) if p == "game.rhai"),
        "{err:?}"
    );
}

#[test]
fn an_entry_outside_the_asset_list_is_rejected() {
    let files = store(&[("game.rhai", SCRIPT.as_bytes())]);
    let json = manifest(
        "other.rhai",
        &[("game.rhai", content_id(SCRIPT.as_bytes()).to_hex())],
    );
    let err = load_bundle(&json, |p| files.get(p).cloned()).unwrap_err();
    assert!(
        matches!(err, BundleError::EntryNotListed(ref p) if p == "other.rhai"),
        "{err:?}"
    );
}

#[test]
fn a_duplicate_asset_path_is_rejected() {
    let id = content_id(SCRIPT.as_bytes()).to_hex();
    let files = store(&[("game.rhai", SCRIPT.as_bytes())]);
    let json = manifest("game.rhai", &[("game.rhai", id.clone()), ("game.rhai", id)]);
    let err = load_bundle(&json, |p| files.get(p).cloned()).unwrap_err();
    assert!(
        matches!(err, BundleError::DuplicateAsset(ref p) if p == "game.rhai"),
        "{err:?}"
    );
}

#[test]
fn a_malformed_manifest_or_id_is_rejected() {
    let none = |_: &str| None;
    assert!(matches!(
        load_bundle("{ not json", none),
        Err(BundleError::Manifest(_))
    ));
    let json = manifest("game.rhai", &[("game.rhai", "not-hex".to_string())]);
    assert!(matches!(
        load_bundle(&json, none),
        Err(BundleError::Manifest(_))
    ));
}

#[test]
fn a_non_utf8_entry_is_rejected() {
    let bytes: &[u8] = b"\xff\xfe";
    let files = store(&[("game.rhai", bytes)]);
    let json = manifest("game.rhai", &[("game.rhai", content_id(bytes).to_hex())]);
    let err = load_bundle(&json, |p| files.get(p).cloned()).unwrap_err();
    assert!(matches!(err, BundleError::EntryNotUtf8(_)), "{err:?}");
}

/// Asset paths are relative, inside the bundle: no parent directories, no absolute paths, no
/// backslashes, nothing empty.
#[test]
fn paths_that_escape_the_bundle_are_rejected() {
    for bad in ["../secret", "a/../../b", "/etc/passwd", "a\\b", "", "./"] {
        let json = manifest(bad, &[(bad, content_id(b"x").to_hex())]);
        let err = load_bundle(&json, |_| Some(b"x".to_vec())).unwrap_err();
        assert!(matches!(err, BundleError::BadPath(_)), "{bad:?}: {err:?}");
    }
}

/// The sample game in the repository is a valid bundle (its manifest hash matches its script).
#[test]
fn the_sample_bundle_loads_from_disk() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/sample/manifest.json");
    let bundle = load_bundle_dir(&path).unwrap();
    assert_eq!(bundle.entry, "arena.rhai");
    assert!(bundle.entry_source.contains("fn update(a)"));
}

// ── Audit follow-ups (2026-10-21): the on-disk loader, Windows names ──

/// Reserved Windows device names and segments ending in a dot or space are not plain paths.
#[test]
fn windows_device_names_are_rejected() {
    for bad in [
        "CON",
        "nul",
        "aux.rhai",
        "art/COM1.txt",
        "lpt9",
        "a.",
        "a ",
        "...",
    ] {
        let json = manifest(bad, &[(bad, content_id(b"x").to_hex())]);
        let err = load_bundle(&json, |_| Some(b"x".to_vec())).unwrap_err();
        assert!(matches!(err, BundleError::BadPath(_)), "{bad:?}: {err:?}");
    }
}

#[cfg(unix)]
mod on_disk {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A bundle directory with `manifest.json` listing `name` with the id of `secret`.
    fn bundle_dir(name: &str, secret: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            manifest(name, &[(name, content_id(secret).to_hex())]),
        )
        .unwrap();
        dir
    }

    /// A symlinked asset pointing outside the bundle is not read, even when its hash matches.
    #[test]
    fn a_symlink_to_a_file_outside_the_bundle_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        let secret = b"SECRET OUTSIDE THE BUNDLE";
        std::fs::write(outside.path().join("secret.txt"), secret).unwrap();
        let dir = bundle_dir("leak.txt", secret);
        symlink(
            outside.path().join("secret.txt"),
            dir.path().join("leak.txt"),
        )
        .unwrap();
        let err = load_bundle_dir(&dir.path().join("manifest.json")).unwrap_err();
        assert!(matches!(err, BundleError::MissingAsset(_)), "{err:?}");
    }

    /// A symlinked directory inside the bundle does not let a path escape through it.
    #[test]
    fn a_symlinked_directory_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        let secret = b"through the link";
        std::fs::write(outside.path().join("f.txt"), secret).unwrap();
        let dir = bundle_dir("root/f.txt", secret);
        symlink(outside.path(), dir.path().join("root")).unwrap();
        let err = load_bundle_dir(&dir.path().join("manifest.json")).unwrap_err();
        assert!(matches!(err, BundleError::MissingAsset(_)), "{err:?}");
    }

    /// A FIFO (or any non-regular file) is refused at once instead of blocking the loader.
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = bundle_dir("pipe", b"x");
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("SKIPPED: mkfifo is unavailable");
            return;
        }
        let path = dir.path().join("manifest.json");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_bundle_dir(&path).is_err());
        });
        let refused = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the loader blocked on a FIFO");
        assert!(refused);
    }

    /// A regular file inside the bundle still loads.
    #[test]
    fn a_regular_file_in_a_subdirectory_loads() {
        let dir = bundle_dir("art/a.rhai", b"fn update(a) { a }");
        std::fs::create_dir(dir.path().join("art")).unwrap();
        std::fs::write(dir.path().join("art/a.rhai"), b"fn update(a) { a }").unwrap();
        let bundle = load_bundle_dir(&dir.path().join("manifest.json")).unwrap();
        assert_eq!(bundle.entry_source, "fn update(a) { a }");
    }
}

// ── Re-review follow-ups (2026-10-21) ──

/// More reserved Windows names: COM0/LPT0, the superscript-digit forms, CONIN$/CONOUT$, and a
/// space before the extension (Windows strips it).
#[test]
fn more_windows_device_names_are_rejected() {
    for bad in [
        "COM0",
        "lpt0",
        "COM¹",
        "LPT²",
        "lpt³",
        "CONIN$",
        "conout$.txt",
        "con .rhai",
        "nul .txt",
    ] {
        let json = manifest(bad, &[(bad, content_id(b"x").to_hex())]);
        let err = load_bundle(&json, |_| Some(b"x".to_vec())).unwrap_err();
        assert!(matches!(err, BundleError::BadPath(_)), "{bad:?}: {err:?}");
    }
}

/// A manifest named by a bare relative path (`UNIBLOX_BUNDLE=manifest.json`) loads from the current
/// directory. The test changes the working directory, so it runs in its own process.
#[test]
fn a_bare_relative_manifest_path_loads() {
    if std::env::var_os("BUNDLE_RELATIVE_CHILD").is_some() {
        let bundle = load_bundle_dir(Path::new("manifest.json")).unwrap();
        assert_eq!(bundle.entry_source, SCRIPT);
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rhai"), SCRIPT).unwrap();
    std::fs::write(
        dir.path().join("manifest.json"),
        manifest(
            "a.rhai",
            &[("a.rhai", content_id(SCRIPT.as_bytes()).to_hex())],
        ),
    )
    .unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_bare_relative_manifest_path_loads",
            "--nocapture",
        ])
        .env("BUNDLE_RELATIVE_CHILD", "1")
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
}
