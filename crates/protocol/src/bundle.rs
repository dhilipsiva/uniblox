//! Content bundles (ADR-0045): the publishable unit of user-generated content. A JSON manifest names
//! the `{engine, content, schema}` [`VersionTriple`], the entry-point Rhai script, and every asset by
//! its blake3 [`ContentId`]; [`load_bundle`] returns the assets only when every id matches. Bundles
//! come from players, so the loader treats them as untrusted: asset paths must stay inside the bundle
//! (relative, no `..`, no backslashes, drive prefixes or control characters), and a wrong hash, a missing or duplicate
//! asset, or an entry that is not listed is an error. Size caps before compile are part of the
//! sandbox hardening (TODO.md Phase 12).
//!
//! ```json
//! { "version": { "engine": 1, "content": 1, "schema": 1 },
//!   "entry": "arena.rhai",
//!   "assets": [{ "path": "arena.rhai", "id": "<blake3 hex>" }] }
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;

use crate::{ContentId, VersionTriple, content_id};

/// One asset a manifest lists: a bundle-relative path and the blake3 id of its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleAsset {
    pub path: String,
    pub id: ContentId,
}

/// A parsed and validated manifest: the paths are safe, unique, and include the entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleManifest {
    pub version: VersionTriple,
    pub entry: String,
    pub assets: Vec<BundleAsset>,
}

/// A loaded bundle: every asset's bytes matched its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    pub version: VersionTriple,
    /// The entry script's bundle-relative path.
    pub entry: String,
    /// The entry script's source (UTF-8).
    pub entry_source: String,
    /// Every asset, by path, including the entry.
    pub assets: BTreeMap<String, Vec<u8>>,
}

/// Why a bundle was rejected.
#[derive(Debug)]
pub enum BundleError {
    /// The manifest is not valid JSON of the expected shape, or an id is not 64 hex digits.
    Manifest(String),
    /// A path is not a plain relative path inside the bundle.
    BadPath(String),
    /// Two assets share a path.
    DuplicateAsset(String),
    /// The entry script is not in the asset list.
    EntryNotListed(String),
    /// A listed asset could not be read.
    MissingAsset(String),
    /// An asset's bytes do not hash to the listed id.
    HashMismatch { path: String },
    /// The entry script is not UTF-8.
    EntryNotUtf8(String),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BundleError::Manifest(why) => write!(f, "invalid bundle manifest: {why}"),
            BundleError::BadPath(p) => {
                write!(f, "asset path {p:?} is not a plain path inside the bundle")
            }
            BundleError::DuplicateAsset(p) => write!(f, "asset {p:?} is listed twice"),
            BundleError::EntryNotListed(p) => write!(f, "entry {p:?} is not in the asset list"),
            BundleError::MissingAsset(p) => write!(f, "asset {p:?} could not be read"),
            BundleError::HashMismatch { path } => {
                write!(f, "asset {path:?} does not match its listed id")
            }
            BundleError::EntryNotUtf8(p) => write!(f, "entry {p:?} is not UTF-8"),
        }
    }
}

impl std::error::Error for BundleError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    version: VersionTriple,
    entry: String,
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAsset {
    path: String,
    id: String,
}

/// A plain relative path inside the bundle: `/`-separated, every segment non-empty and neither `.`
/// nor `..`, no backslash, no `:` (drive or scheme prefixes), no control characters, not absolute.
fn check_path(path: &str) -> Result<(), BundleError> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..");
    if bad {
        Err(BundleError::BadPath(path.to_string()))
    } else {
        Ok(())
    }
}

impl BundleManifest {
    /// Parse and validate a JSON manifest.
    pub fn parse(json: &str) -> Result<Self, BundleError> {
        let raw: RawManifest =
            serde_json::from_str(json).map_err(|e| BundleError::Manifest(e.to_string()))?;
        let mut seen = BTreeSet::new();
        let mut assets = Vec::with_capacity(raw.assets.len());
        for a in raw.assets {
            check_path(&a.path)?;
            let id = ContentId::from_hex(&a.id)
                .map_err(|e| BundleError::Manifest(format!("asset {:?}: {e}", a.path)))?;
            if !seen.insert(a.path.clone()) {
                return Err(BundleError::DuplicateAsset(a.path));
            }
            assets.push(BundleAsset { path: a.path, id });
        }
        check_path(&raw.entry)?;
        if !seen.contains(&raw.entry) {
            return Err(BundleError::EntryNotListed(raw.entry));
        }
        Ok(Self {
            version: raw.version,
            entry: raw.entry,
            assets,
        })
    }
}

/// Load a bundle: parse the manifest, `read` each listed asset by its bundle-relative path, and verify
/// every asset against its id. Nothing is returned unless every asset matches.
pub fn load_bundle(
    manifest_json: &str,
    mut read: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Result<Bundle, BundleError> {
    let manifest = BundleManifest::parse(manifest_json)?;
    let mut assets = BTreeMap::new();
    for asset in &manifest.assets {
        let bytes =
            read(&asset.path).ok_or_else(|| BundleError::MissingAsset(asset.path.clone()))?;
        if content_id(&bytes) != asset.id {
            return Err(BundleError::HashMismatch {
                path: asset.path.clone(),
            });
        }
        assets.insert(asset.path.clone(), bytes);
    }
    let entry_bytes = assets
        .get(&manifest.entry)
        .ok_or_else(|| BundleError::EntryNotListed(manifest.entry.clone()))?;
    let entry_source = String::from_utf8(entry_bytes.clone())
        .map_err(|_| BundleError::EntryNotUtf8(manifest.entry.clone()))?;
    Ok(Bundle {
        version: manifest.version,
        entry: manifest.entry,
        entry_source,
        assets,
    })
}

/// Native convenience: load the bundle whose manifest is the file at `manifest_path`, reading assets
/// relative to the manifest's directory. The path checks in [`BundleManifest::parse`] keep every read
/// inside that directory.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_bundle_dir(manifest_path: &std::path::Path) -> Result<Bundle, BundleError> {
    let json = std::fs::read_to_string(manifest_path)
        .map_err(|e| BundleError::Manifest(format!("{}: {e}", manifest_path.display())))?;
    let dir = manifest_path.parent().unwrap_or(std::path::Path::new("."));
    load_bundle(&json, |rel| std::fs::read(dir.join(rel)).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_path_rejects_escapes() {
        for bad in ["a\\b", "c:x", "/a", "a//b", "a/./b", "../a", "a\u{8}b", ""] {
            assert!(
                matches!(check_path(bad), Err(BundleError::BadPath(_))),
                "{bad:?}"
            );
        }
        for good in ["arena.rhai", "art/logo.png", "a.b/c-d_e"] {
            assert!(check_path(good).is_ok(), "{good:?}");
        }
    }
}
