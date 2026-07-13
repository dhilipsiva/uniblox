//! Native durable content-addressed store (ADR-0034): a directory where each
//! save blob is a file `<content-id-hex>.blob`. Native-only (`std::fs`); the
//! browser durable store is B4 (`IdbStore`, IndexedDB).
//!
//! `FileStore` exposes INHERENT fallible methods (`std::io::Result`) rather than
//! implementing the infallible sync [`crate::ContentStore`] trait: file I/O
//! genuinely fails (permissions, disk full), and swallowing that into the
//! infallible trait would hide real errors. The trait stays the in-memory
//! abstraction ([`crate::MemoryStore`]); durable backends get backend-shaped
//! APIs (B4's IndexedDB store is fallible AND async).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use protocol::{ContentId, content_id};

/// Monotonic per-process counter for unique temp-file names, so two writers
/// never collide on the same temp path (the final `.blob` name is
/// content-addressed and reached only via an atomic rename).
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A content-addressed blob store backed by a directory: each blob is written to
/// `<root>/<content-id-hex>.blob`. Content-addressed ⇒ writes are idempotent and
/// durable across process restarts. A corrupt/partial file (were one to occur)
/// is caught downstream — `load_world_verified` → `ContentMismatch`, or
/// `load_world` → `Codec` — never silently loaded as valid.
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    /// Open a store rooted at `root`, creating the directory (and parents) if
    /// needed.
    pub fn open(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(FileStore { root })
    }

    /// The on-disk path for a blob id.
    fn path_for(&self, id: ContentId) -> PathBuf {
        self.root.join(format!("{}.blob", id.to_hex()))
    }

    /// Store `blob` under its content id and return that id. Idempotent: a
    /// present file is left untouched (content-addressed ⇒ same id ⇒ same bytes).
    /// Writes to a temp file then atomically renames it into place, so a crash
    /// mid-write never leaves a partial file under the final name.
    pub fn put(&self, blob: &[u8]) -> io::Result<ContentId> {
        let id = content_id(blob);
        let final_path = self.path_for(id);
        if final_path.exists() {
            return Ok(id); // dedup — identical content already stored
        }
        // Unique temp name (per process + call) so concurrent writers don't
        // clobber each other's temp; the rename target is still the shared
        // content-addressed final path.
        let tmp = self.root.join(format!(
            "{}.{}.{}.blob.tmp",
            id.to_hex(),
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&tmp, blob)?;
        fs::rename(&tmp, &final_path)?; // atomic on the same filesystem
        Ok(id)
    }

    /// Fetch the blob for `id`, or `Ok(None)` if absent (a cache miss — treated
    /// as evictable, not an error). Genuine I/O failures surface as `Err`.
    pub fn get(&self, id: ContentId) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path_for(id)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether a blob for `id` is present.
    pub fn contains(&self, id: ContentId) -> bool {
        self.path_for(id).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SaveError, load_world_verified, save_world};
    use bevy_ecs::world::World;
    use engine_core::{Position, Tick, Velocity, insert_sim, spawn_owned};
    use protocol::PeerId;

    const DT: f32 = 1.0 / 64.0;
    const LOCAL: PeerId = PeerId(1);

    #[test]
    fn put_get_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileStore::open(dir.path()).expect("open");
        let blob = b"hello save".to_vec();

        let id = store.put(&blob).expect("put");
        assert_eq!(id, content_id(&blob));
        assert_eq!(store.get(id).expect("get"), Some(blob));
    }

    #[test]
    fn durable_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blob = b"persist me".to_vec();

        let id = {
            let store = FileStore::open(dir.path()).expect("open");
            store.put(&blob).expect("put")
        }; // first store dropped

        let reopened = FileStore::open(dir.path()).expect("reopen");
        assert_eq!(reopened.get(id).expect("get"), Some(blob));
    }

    #[test]
    fn idempotent_reput_writes_one_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileStore::open(dir.path()).expect("open");
        let blob = b"twice".to_vec();

        let id1 = store.put(&blob).expect("put1");
        let id2 = store.put(&blob).expect("put2");
        assert_eq!(id1, id2);

        let blobs = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "blob"))
            .count();
        assert_eq!(blobs, 1); // no lingering temp; exactly one .blob
    }

    #[test]
    fn missing_and_contains() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileStore::open(dir.path()).expect("open");
        let present = store.put(b"here").expect("put");
        let absent = content_id(b"not stored");

        assert_eq!(store.get(absent).expect("get"), None);
        assert!(!store.contains(absent));
        assert!(store.contains(present));
    }

    fn world_with(tick: u64, pos: Position) -> World {
        let mut w = World::new();
        insert_sim(&mut w, LOCAL, DT);
        w.insert_resource(Tick(tick));
        spawn_owned(&mut w, LOCAL, pos, Velocity { x: 0.5, y: 0.0 });
        w
    }

    #[test]
    fn save_world_round_trips_through_the_file_store() {
        let w = world_with(9, Position { x: 3.0, y: -1.0 });
        let (id, blob) = save_world(&w).expect("save");

        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileStore::open(dir.path()).expect("open");
        assert_eq!(store.put(&blob).expect("put"), id);

        let mut w2 = World::new();
        let bytes = store.get(id).expect("get").expect("present");
        load_world_verified(&mut w2, id, &bytes, DT).expect("verified load");
        // Fidelity: re-saving the reloaded world yields the same id.
        assert_eq!(save_world(&w2).expect("resave").0, id);
    }

    #[test]
    fn tampered_on_disk_blob_is_caught_by_verified_load() {
        let w = world_with(1, Position { x: 0.0, y: 0.0 });
        let (id, blob) = save_world(&w).expect("save");

        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileStore::open(dir.path()).expect("open");
        store.put(&blob).expect("put");

        // Corrupt the on-disk file behind the id's back.
        let path = store.path_for(id);
        let mut bytes = std::fs::read(&path).expect("read");
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&path, &bytes).expect("write");

        let mut w2 = World::new();
        let got = store.get(id).expect("get").expect("present");
        assert!(matches!(
            load_world_verified(&mut w2, id, &got, DT),
            Err(SaveError::ContentMismatch { .. })
        ));
    }
}
