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
    /// A file on disk is larger than the loader reads.
    TooLarge { path: String, limit: u64 },
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
            BundleError::TooLarge { path, limit } => {
                write!(f, "{path:?} is larger than the {limit}-byte limit")
            }
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

/// Windows device names, reserved in any directory and with any extension (`aux.rhai` opens `AUX`).
const WINDOWS_DEVICES: &[&str] = &["con", "prn", "aux", "nul", "conin$", "conout$"];

/// `COM` or `LPT` followed by one digit, including `0` and the superscripts Windows also reserves.
fn is_port_device(stem: &str) -> bool {
    let rest = stem
        .strip_prefix("com")
        .or_else(|| stem.strip_prefix("lpt"));
    let mut chars = rest.unwrap_or("").chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('0'..='9' | '¹' | '²' | '³'), None)
    )
}

/// One path segment that is safe on every platform: non-empty, not `.` or `..`, not ending in a
/// dot or a space (Windows strips them), and not a Windows device name.
fn plain_segment(seg: &str) -> bool {
    // Windows ignores spaces before the extension too: `con .rhai` opens `CON`.
    let stem = seg
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_lowercase();
    !seg.is_empty()
        && seg != "."
        && seg != ".."
        && !seg.ends_with('.')
        && !seg.ends_with(' ')
        && !WINDOWS_DEVICES.contains(&stem.as_str())
        && !is_port_device(&stem)
}

/// A plain relative path inside the bundle: `/`-separated plain segments ([`plain_segment`]), no
/// backslash, no `:` (drive or scheme prefixes), no control characters, not absolute.
fn check_path(path: &str) -> Result<(), BundleError> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(char::is_control)
        || !path.split('/').all(plain_segment);
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
    load_with(manifest_json, |path| {
        read(path).ok_or_else(|| BundleError::MissingAsset(path.to_string()))
    })
}

fn load_with(
    manifest_json: &str,
    mut read: impl FnMut(&str) -> Result<Vec<u8>, BundleError>,
) -> Result<Bundle, BundleError> {
    let manifest = BundleManifest::parse(manifest_json)?;
    let mut assets = BTreeMap::new();
    for asset in &manifest.assets {
        let bytes = read(&asset.path)?;
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

/// The most [`load_bundle_dir`] reads for one asset (64 MiB) and for the manifest (1 MiB).
pub const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Read one regular file, refusing symlinks, special files (FIFOs, devices), anything that resolves
/// outside `root`, and anything over `limit` bytes. Any refusal except size reads as missing.
#[cfg(not(target_arch = "wasm32"))]
fn read_regular(
    root: &std::path::Path,
    path: &std::path::Path,
    label: &str,
    limit: u64,
) -> Result<Vec<u8>, BundleError> {
    use std::io::Read;
    let missing = || BundleError::MissingAsset(label.to_string());
    // `symlink_metadata` does not follow a final symlink; `canonicalize` resolves every one, so a
    // symlinked parent directory cannot lead the read outside the bundle either.
    let meta = std::fs::symlink_metadata(path).map_err(|_| missing())?;
    if !meta.file_type().is_file() {
        return Err(missing());
    }
    let real = path.canonicalize().map_err(|_| missing())?;
    if !real.starts_with(root) {
        return Err(missing());
    }
    if meta.len() > limit {
        return Err(BundleError::TooLarge {
            path: label.to_string(),
            limit,
        });
    }
    let file = std::fs::File::open(&real).map_err(|_| missing())?;
    // Re-check on the open handle: a file swapped for something else after the path checks is
    // refused here. (Swapping in a FIFO between the checks and `open` could still block; a writer
    // racing the loader inside the bundle directory is outside this local loader's threat model.)
    if !file.metadata().map_err(|_| missing())?.is_file() {
        return Err(missing());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| missing())?;
    if bytes.len() as u64 > limit {
        return Err(BundleError::TooLarge {
            path: label.to_string(),
            limit,
        });
    }
    Ok(bytes)
}

/// Native convenience: load the bundle whose manifest is the file at `manifest_path`, reading assets
/// relative to the manifest's directory. Only regular files that resolve inside that directory are
/// read (no symlinks, FIFOs or devices), each at most [`MAX_ASSET_BYTES`]; the manifest itself at most
/// [`MAX_MANIFEST_BYTES`]. A hard link to a file outside the bundle cannot be told apart by path and is
/// read; its bytes must still match the listed id. There is no total cap across assets yet (TODO.md
/// Phase 12).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_bundle_dir(manifest_path: &std::path::Path) -> Result<Bundle, BundleError> {
    let manifest_err =
        |e: &dyn fmt::Display| BundleError::Manifest(format!("{}: {e}", manifest_path.display()));
    // A bare file name has an empty parent: that is the current directory.
    let dir = match manifest_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };
    let root = dir.canonicalize().map_err(|e| manifest_err(&e))?;
    let json =
        read_regular(&root, manifest_path, "manifest", MAX_MANIFEST_BYTES).map_err(
            |e| match e {
                BundleError::MissingAsset(_) => {
                    manifest_err(&"not a readable regular file in the bundle")
                }
                other => other,
            },
        )?;
    let json = String::from_utf8(json).map_err(|e| manifest_err(&e))?;
    load_with(&json, |rel| {
        read_regular(&root, &dir.join(rel), rel, MAX_ASSET_BYTES)
    })
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
