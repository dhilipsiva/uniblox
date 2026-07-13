//! Browser durable content-addressed store (ADR-0035): IndexedDB, keyed by
//! `ContentId` hex, value = the blob bytes. The wasm counterpart to the native
//! `FileStore` — a Mode-1 save survives a page reload.
//!
//! IndexedDB is ASYNC and event-callback based (an `IDBRequest`/`IDBTransaction`
//! fires `success`/`complete`/`error` events, NOT a Promise), so this is written
//! against raw `web-sys` (chosen over a helper crate to keep the exact
//! `wasm-bindgen` pin safe) with a small hand-rolled event→`oneshot` bridge.
//! Like `FileStore`, it exposes async, fallible ([`IdbError`]) methods and does
//! NOT implement the infallible sync `crate::ContentStore` trait.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use futures::channel::oneshot;
use js_sys::Uint8Array;
use protocol::{ContentId, content_id};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    IdbDatabase, IdbFactory, IdbObjectStore, IdbOpenDbRequest, IdbRequest, IdbTransaction,
    IdbTransactionMode,
};

/// An IndexedDB error, stringified so `JsValue`/web-sys types never leak into
/// the public API (callers handle a plain message).
#[derive(Debug, Clone)]
pub struct IdbError(String);

impl IdbError {
    fn msg(s: impl Into<String>) -> Self {
        IdbError(s.into())
    }
    fn from_js(v: JsValue) -> Self {
        IdbError(format!("{v:?}"))
    }
}

impl fmt::Display for IdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IndexedDB error: {}", self.0)
    }
}

impl std::error::Error for IdbError {}

/// Await an `IDBRequest`: resolve with `req.result()` on `success`, reject on
/// `error`. The event closures are bound locals kept alive across the `.await`
/// (only one of success/error fires; a `oneshot` carries the outcome).
async fn await_request(req: &IdbRequest) -> Result<JsValue, IdbError> {
    let (tx, rx) = oneshot::channel::<Result<JsValue, IdbError>>();
    let tx = Rc::new(RefCell::new(Some(tx)));

    let req_ok = req.clone();
    let tx_ok = tx.clone();
    let onsuccess = Closure::<dyn FnMut()>::new(move || {
        if let Some(tx) = tx_ok.borrow_mut().take() {
            let _ = tx.send(req_ok.result().map_err(IdbError::from_js));
        }
    });

    let req_err = req.clone();
    let tx_err = tx.clone();
    let onerror = Closure::<dyn FnMut()>::new(move || {
        if let Some(tx) = tx_err.borrow_mut().take() {
            let _ = tx.send(Err(request_error(&req_err)));
        }
    });

    req.set_onsuccess(Some(onsuccess.as_ref().unchecked_ref()));
    req.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let out = rx
        .await
        .map_err(|_| IdbError::msg("IndexedDB request was dropped"))?;

    // The event has fired; detach the handlers before the closures drop.
    req.set_onsuccess(None);
    req.set_onerror(None);
    drop(onsuccess);
    drop(onerror);
    out
}

/// Read an `IDBRequest`'s `error` into a message.
fn request_error(req: &IdbRequest) -> IdbError {
    match req.error() {
        Ok(Some(e)) => IdbError(format!("{}: {}", e.name(), e.message())),
        _ => IdbError::msg("unknown IndexedDB request error"),
    }
}

/// Await an `IDBTransaction`'s commit: resolve on `complete`, reject on
/// `error`/`abort`. Used by [`IdbStore::put`] so a value is DURABLE (committed)
/// before we return — a read-back after a reload needs the commit, not just the
/// put request's success.
async fn await_tx(tx_handle: &IdbTransaction) -> Result<(), IdbError> {
    let (sender, rx) = oneshot::channel::<Result<(), IdbError>>();
    let sender = Rc::new(RefCell::new(Some(sender)));

    let s_ok = sender.clone();
    let oncomplete = Closure::<dyn FnMut()>::new(move || {
        if let Some(s) = s_ok.borrow_mut().take() {
            let _ = s.send(Ok(()));
        }
    });
    let s_err = sender.clone();
    let onerror = Closure::<dyn FnMut()>::new(move || {
        if let Some(s) = s_err.borrow_mut().take() {
            let _ = s.send(Err(IdbError::msg("IndexedDB transaction error")));
        }
    });
    let s_abort = sender.clone();
    let onabort = Closure::<dyn FnMut()>::new(move || {
        if let Some(s) = s_abort.borrow_mut().take() {
            let _ = s.send(Err(IdbError::msg("IndexedDB transaction aborted")));
        }
    });

    tx_handle.set_oncomplete(Some(oncomplete.as_ref().unchecked_ref()));
    tx_handle.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    tx_handle.set_onabort(Some(onabort.as_ref().unchecked_ref()));

    let out = rx
        .await
        .map_err(|_| IdbError::msg("IndexedDB transaction was dropped"))?;

    tx_handle.set_oncomplete(None);
    tx_handle.set_onerror(None);
    tx_handle.set_onabort(None);
    drop(oncomplete);
    drop(onerror);
    drop(onabort);
    out
}

/// A content-addressed IndexedDB store: one object store keyed by
/// `ContentId::to_hex()`, values are the blob bytes (`Uint8Array`).
pub struct IdbStore {
    db: IdbDatabase,
    store: String,
}

impl IdbStore {
    /// Open (creating if needed) database `db_name` with an object store
    /// `store_name`.
    pub async fn open(db_name: &str, store_name: &str) -> Result<Self, IdbError> {
        let factory: IdbFactory = web_sys::window()
            .ok_or_else(|| IdbError::msg("no window"))?
            .indexed_db()
            .map_err(IdbError::from_js)?
            .ok_or_else(|| IdbError::msg("IndexedDB unavailable"))?;

        // The DB version is permanently 1, so `upgradeneeded` fires exactly once
        // (at first creation) and `blocked` (a stale older connection during a
        // version bump) can never fire — `await_request` needs no `blocked` arm.
        let open_req: IdbOpenDbRequest = factory
            .open_with_u32(db_name, 1)
            .map_err(IdbError::from_js)?;

        // Create the object store on first creation. Capture a create FAILURE so
        // `open` returns Err rather than silently committing a store-less v1 DB
        // (which, with the version pinned at 1, would never self-heal).
        let store_owned = store_name.to_string();
        let req_for_upgrade = open_req.clone();
        let create_err: Rc<RefCell<Option<IdbError>>> = Rc::new(RefCell::new(None));
        let create_err_cb = create_err.clone();
        let onupgrade = Closure::<dyn FnMut()>::new(move || {
            if let Ok(result) = req_for_upgrade.result()
                && let Ok(db) = result.dyn_into::<IdbDatabase>()
                && let Err(e) = db.create_object_store(&store_owned)
            {
                *create_err_cb.borrow_mut() = Some(IdbError::from_js(e));
            }
        });
        open_req.set_onupgradeneeded(Some(onupgrade.as_ref().unchecked_ref()));

        let result = await_request(open_req.as_ref()).await?;
        open_req.set_onupgradeneeded(None);
        drop(onupgrade);
        if let Some(e) = create_err.borrow_mut().take() {
            return Err(e);
        }

        let db: IdbDatabase = result
            .dyn_into()
            .map_err(|_| IdbError::msg("open did not yield an IdbDatabase"))?;
        Ok(IdbStore {
            db,
            store: store_name.to_string(),
        })
    }

    /// Store `blob` under its content id and return that id. Awaits the
    /// transaction commit, so the value is durable on return.
    pub async fn put(&self, blob: &[u8]) -> Result<ContentId, IdbError> {
        let id = content_id(blob);
        let key = JsValue::from_str(&id.to_hex());
        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store, IdbTransactionMode::Readwrite)
            .map_err(IdbError::from_js)?;
        let store: IdbObjectStore = tx.object_store(&self.store).map_err(IdbError::from_js)?;
        let value = Uint8Array::from(blob);
        // A put error surfaces via the transaction's error/abort (awaited below).
        store
            .put_with_key(value.as_ref(), &key)
            .map_err(IdbError::from_js)?;
        await_tx(&tx).await?;
        Ok(id)
    }

    /// Fetch the blob for `id`, or `Ok(None)` if absent (an evictable miss).
    pub async fn get(&self, id: ContentId) -> Result<Option<Vec<u8>>, IdbError> {
        let key = JsValue::from_str(&id.to_hex());
        let tx = self
            .db
            .transaction_with_str(&self.store)
            .map_err(IdbError::from_js)?;
        let store: IdbObjectStore = tx.object_store(&self.store).map_err(IdbError::from_js)?;
        let req = store.get(&key).map_err(IdbError::from_js)?;
        let result = await_request(&req).await?;
        if result.is_undefined() || result.is_null() {
            return Ok(None);
        }
        let arr: Uint8Array = result
            .dyn_into()
            .map_err(|_| IdbError::msg("stored value was not a Uint8Array"))?;
        Ok(Some(arr.to_vec()))
    }

    /// Whether a blob for `id` is present.
    pub async fn contains(&self, id: ContentId) -> Result<bool, IdbError> {
        Ok(self.get(id).await?.is_some())
    }
}
