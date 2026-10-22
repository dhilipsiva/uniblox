//! Time-limited TURN credentials (ADR-0050), in coturn's REST-API scheme (`use-auth-secret` with a
//! `static-auth-secret`): the username is `<expiry>:<label>` and the password is the standard base64
//! of HMAC-SHA1(secret, username). coturn recomputes it from the shared secret and refuses it once the
//! expiry has passed, so no per-user state is kept anywhere.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// The longest lifetime a TURN credential is minted with (a day).
pub const MAX_TURN_TTL_SECS: u64 = 24 * 3600;

/// The TURN relay the signaling service hands credentials for.
#[derive(Clone)]
pub struct TurnConfig {
    /// coturn's `static-auth-secret`.
    pub secret: Vec<u8>,
    /// The `turn:`/`turns:` URLs a client should use.
    pub urls: Vec<String>,
    /// How long a credential stays valid (capped at [`MAX_TURN_TTL_SECS`]).
    pub ttl: Duration,
}

impl fmt::Debug for TurnConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnConfig")
            .field("secret", &"..")
            .field("urls", &self.urls)
            .field("ttl", &self.ttl)
            .finish()
    }
}

/// One TURN username/password pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnCredential {
    pub username: String,
    pub credential: String,
}

/// When a credential minted at `now` (unix seconds) for a join token expiring at `token_expiry`
/// expires. The expiry is rounded down to a grid of half the TTL, so a token keeps one username for
/// a grid step (coturn's per-user quota binds per username), and it never outlives the token.
pub fn credential_expiry(now: u64, ttl: Duration, token_expiry: u64) -> u64 {
    let ttl_secs = ttl.as_secs().clamp(1, MAX_TURN_TTL_SECS);
    let step = (ttl_secs / 2).max(1);
    ((now + ttl_secs) / step * step).min(token_expiry)
}

/// The credential for `label` that coturn accepts until `expires` (unix seconds).
pub fn turn_credential(secret: &[u8], label: &str, expires: u64) -> TurnCredential {
    let username = format!("{expires}:{label}");
    // HMAC accepts a key of any length; `new_from_slice` cannot fail for it.
    let tag = match Hmac::<Sha1>::new_from_slice(secret) {
        Ok(mut mac) => {
            mac.update(username.as_bytes());
            mac.finalize().into_bytes().to_vec()
        }
        Err(_) => Vec::new(),
    };
    TurnCredential {
        credential: STANDARD.encode(tag),
        username,
    }
}
