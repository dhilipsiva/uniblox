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
