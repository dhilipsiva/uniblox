//! Mode 2 (P2P) in the browser (ADR-0051): with `?mode2=<room>&signal=<http base>` the page joins a
//! scoped room as a peer, through the authenticated signaling service (a join token from its
//! endpoint, ADR-0047), and runs `server::build_peer_app`: the same app, systems and pump as the
//! Mode-3 server, owning only what it spawns. Its signing key comes from `crypto.getRandomValues`.
//!
//! Optional parameters: `engine` (the client's engine version, default 1), `next` (a session-size
//! cap), `entities` (how many entities to spawn, default 1), `arbitrate` (the coordinator's
//! arbitration window in ms, default 0). Setting `location.hash` to a new `#claim…` value claims
//! the first entity (by network id) the coordinator owns.
//!
//! Console markers, for `scripts/browser-e2e.py`: `[uniblox-mode2] id <peer>`, `entities <n>`,
//! `coordinator <peer>`, `owner <spawner>/<index> <old> -> <new>`, `claiming <spawner>/<index>`,
//! `claim rejected`, and `failed <reason>`.

use std::collections::BTreeMap;
use std::time::Duration;

use bevy::prelude::*;
use engine_core::{LocalPeer, Owner};
use gloo_timers::future::TimeoutFuture;
use protocol::PeerId;
use transport::Transport;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};

const TAG: &str = "[uniblox-mode2]";

fn log(msg: &str) {
    web_sys::console::log_1(&msg.into());
}

fn js_err(err: JsValue) -> String {
    err.as_string().unwrap_or_else(|| format!("{err:?}"))
}

/// What the page's query asks for.
pub struct Params {
    room: String,
    signal: String,
    engine: u32,
    next: Option<usize>,
    entities: usize,
    arbitrate_ms: u64,
}

/// The Mode-2 parameters, if the page asks for Mode 2 (`?mode2=<room>&signal=<http base>`).
pub fn params() -> Option<Params> {
    let search = web_sys::window()?.location().search().ok()?;
    let query = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    let number = |name: &str| query.get(name).and_then(|v| v.parse::<u64>().ok());
    Some(Params {
        room: query.get("mode2")?,
        signal: query.get("signal")?.trim_end_matches('/').to_string(),
        engine: number("engine")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(1),
        next: number("next").and_then(|n| usize::try_from(n).ok()),
        entities: number("entities")
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(1),
        arbitrate_ms: number("arbitrate").unwrap_or(0),
    })
}

/// Start the Mode-2 peer (it runs on the page's event loop; this returns at once).
pub fn start(params: Params) {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    spawn_local(async move {
        if let Err(err) = run(params).await {
            log(&format!("{TAG} failed {err}"));
        }
    });
}

/// A join token for `room` from the signaling service's endpoint (ADR-0047).
async fn fetch_token(signal: &str, room: &str) -> Result<String, String> {
    let window = web_sys::window().ok_or("no window")?;
    let room = String::from(js_sys::encode_uri_component(room));
    let url = format!("{signal}/_uniblox/token?room={room}");
    let response: web_sys::Response = JsFuture::from(window.fetch_with_str(&url))
        .await
        .map_err(js_err)?
        .dyn_into()
        .map_err(js_err)?;
    if !response.ok() {
        return Err(format!("the token endpoint answered {}", response.status()));
    }
    let body = JsFuture::from(response.json().map_err(js_err)?)
        .await
        .map_err(js_err)?;
    js_sys::Reflect::get(&body, &"token".into())
        .ok()
        .and_then(|v| v.as_string())
        .ok_or_else(|| "no token in the endpoint's answer".to_string())
}

/// A signing key from `crypto.getRandomValues` (ADR-0049).
fn random_key() -> Result<replication::SigningKey, String> {
    let mut seed = [0u8; 32];
    web_sys::window()
        .ok_or("no window")?
        .crypto()
        .map_err(js_err)?
        .get_random_values_with_u8_array(&mut seed)
        .map_err(js_err)?;
    Ok(replication::SigningKey::from_bytes(&seed))
}

async fn run(p: Params) -> Result<(), String> {
    let token = fetch_token(&p.signal, &p.room).await?;
    let ws_base = if let Some(rest) = p.signal.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = p.signal.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        return Err(format!("signal must be an http(s) URL, not {:?}", p.signal));
    };
    let room = String::from(js_sys::encode_uri_component(&p.room));
    let token = String::from(js_sys::encode_uri_component(&token));
    let mut url = format!("{ws_base}/{room}?engine={}&token={token}", p.engine);
    if let Some(next) = p.next {
        url.push_str(&format!("&next={next}"));
    }
    let (transport, loop_fut) = Transport::connect(&url);
    spawn_local(async move {
        if let Err(err) = loop_fut.await {
            log(&format!("{TAG} signaling ended: {err:?}"));
        }
    });
    let mut transport = transport;
    let uuid = loop {
        if let Some(id) = transport.id() {
            break id;
        }
        TimeoutFuture::new(20).await;
    };
    let local = PeerId::from_uuid_bytes(*uuid.0.as_bytes());
    log(&format!("{TAG} id {}", local.0));
    let mut app = server::build_peer_app(transport, local, random_key()?, p.entities);
    server::set_arbitration_window(app.world_mut(), Duration::from_millis(p.arbitrate_ms));
    app.add_systems(Update, (report, claim_on_hash));
    // The schedule runner drives the app from timers on wasm; `run` returns.
    app.run();
    Ok(())
}

/// What the page has already reported.
#[derive(Default)]
struct Seen {
    /// (spawner, index) → owner.
    owners: BTreeMap<(u64, u32), u64>,
    entities: usize,
    coordinator: Option<u64>,
    rejected: u64,
}

/// Log the entity count, ownership changes, the coordinator and rejected claims as they change.
fn report(world: &mut World, mut seen: Local<Seen>) {
    let mut q = world.query::<(Entity, &Owner)>();
    let rows: Vec<(Entity, u64)> = q.iter(world).map(|(e, o)| (e, o.0.0)).collect();
    if rows.len() != seen.entities {
        seen.entities = rows.len();
        log(&format!("{TAG} entities {}", rows.len()));
    }
    for (entity, owner) in rows {
        let Some(id) = server::net_id(world, entity) else {
            continue;
        };
        if let Some(old) = seen.owners.insert((id.spawner.0, id.index), owner)
            && old != owner
        {
            log(&format!(
                "{TAG} owner {}/{} {old} -> {owner}",
                id.spawner.0, id.index
            ));
        }
    }
    if let Some(coordinator) = server::coordinator(world)
        && seen.coordinator != Some(coordinator.0)
    {
        seen.coordinator = Some(coordinator.0);
        log(&format!("{TAG} coordinator {}", coordinator.0));
    }
    let rejected = server::rejected_claims(world);
    if rejected > seen.rejected {
        seen.rejected = rejected;
        log(&format!("{TAG} claim rejected"));
    }
}

/// On a new `#claim…` hash, claim the first entity (by network id) the coordinator owns.
fn claim_on_hash(world: &mut World, mut last: Local<String>) {
    let hash = web_sys::window()
        .and_then(|w| w.location().hash().ok())
        .unwrap_or_default();
    if hash == *last {
        return;
    }
    *last = hash.clone();
    if !hash.starts_with("#claim") {
        return;
    }
    let (Some(coordinator), Some(local)) = (
        server::coordinator(world),
        world.get_resource::<LocalPeer>().map(|l| l.0),
    ) else {
        return;
    };
    let mut q = world.query::<(Entity, &Owner)>();
    let mut owned: Vec<((u64, u32), Entity)> = q
        .iter(world)
        .filter(|(_, o)| o.0 == coordinator && o.0 != local)
        .filter_map(|(e, _)| server::net_id(world, e).map(|id| ((id.spawner.0, id.index), e)))
        .collect();
    owned.sort();
    if let Some(&((spawner, index), entity)) = owned.first() {
        log(&format!("{TAG} claiming {spawner}/{index}"));
        server::claim(world, entity);
    }
}
