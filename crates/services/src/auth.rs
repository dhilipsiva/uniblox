//! Join tokens (ADR-0047): a short-lived HMAC-SHA256 tag binding one room path to an expiry, minted
//! by whoever holds the signaling secret (the token endpoint, an operator's `mint-token`, or a
//! library user's own backend) and checked by the signaling gate before a peer may join.
//!
//! Format: `v1.<expiry, unix seconds>.<64 hex digits of HMAC-SHA256(key, "uniblox-join-v1\0" room
//! "\0" expiry)>`. URL-safe as is. The tag is compared in constant time.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The shortest signaling secret accepted, in bytes. Use 32 random bytes; only the length is checked.
pub const MIN_KEY_BYTES: usize = 32;

/// The longest lifetime a token may claim (a day): a token minted further ahead is refused, so a
/// leaked one cannot be reused for long.
pub const MAX_TOKEN_LIFETIME_SECS: u64 = 24 * 3600;

const VERSION: &str = "v1";
const DOMAIN: &[u8] = b"uniblox-join-v1\0";

/// Why a token was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenError {
    /// The signaling secret is shorter than [`MIN_KEY_BYTES`].
    WeakKey,
    /// Not a `v1.<expiry>.<tag>` token.
    Malformed,
    /// The tag does not match this key, room and expiry.
    Invalid,
    /// The expiry has passed.
    Expired,
    /// The expiry is more than [`MAX_TOKEN_LIFETIME_SECS`] away.
    TooLong,
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TokenError::WeakKey => "the signaling secret must be at least 32 bytes",
            TokenError::Malformed => "malformed join token",
            TokenError::Invalid => "invalid join token for this room",
            TokenError::Expired => "expired join token",
            TokenError::TooLong => "join token lifetime too long",
        })
    }
}

impl std::error::Error for TokenError {}

/// The signaling secret, held as a keyed HMAC. `Debug` never shows it.
#[derive(Clone)]
pub struct JoinTokenKey(HmacSha256);

impl fmt::Debug for JoinTokenKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JoinTokenKey(..)")
    }
}

impl JoinTokenKey {
    /// A key from the raw secret; at least [`MIN_KEY_BYTES`].
    pub fn new(secret: &[u8]) -> Result<Self, TokenError> {
        if secret.len() < MIN_KEY_BYTES {
            return Err(TokenError::WeakKey);
        }
        HmacSha256::new_from_slice(secret)
            .map(Self)
            .map_err(|_| TokenError::WeakKey)
    }

    fn mac(&self, room: &str, expires: u64) -> HmacSha256 {
        let mut mac = self.0.clone();
        mac.update(DOMAIN);
        mac.update(room.as_bytes());
        mac.update(b"\0");
        mac.update(expires.to_string().as_bytes());
        mac
    }

    /// A token for `room` that expires at `expires` (unix seconds).
    pub fn mint(&self, room: &str, expires: u64) -> String {
        let tag = self.mac(room, expires).finalize().into_bytes();
        format!("{VERSION}.{expires}.{}", hex(&tag))
    }

    /// Check `token` for `room` at time `now` (unix seconds).
    pub fn verify(&self, token: &str, room: &str, now: u64) -> Result<(), TokenError> {
        self.verify_expiry(token, room, now).map(|_| ())
    }

    /// [`Self::verify`], returning the token's expiry (unix seconds).
    pub fn verify_expiry(&self, token: &str, room: &str, now: u64) -> Result<u64, TokenError> {
        let mut parts = token.split('.');
        let (Some(VERSION), Some(expires), Some(tag), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        // One spelling per token: digits only, no leading zero.
        if expires.is_empty()
            || !expires.bytes().all(|b| b.is_ascii_digit())
            || (expires.len() > 1 && expires.starts_with('0'))
        {
            return Err(TokenError::Malformed);
        }
        let expires: u64 = expires.parse().map_err(|_| TokenError::Malformed)?;
        let tag = unhex(tag).ok_or(TokenError::Malformed)?;
        self.mac(room, expires)
            .verify_slice(&tag)
            .map_err(|_| TokenError::Invalid)?;
        if now > expires {
            return Err(TokenError::Expired);
        }
        if expires - now > MAX_TOKEN_LIFETIME_SECS {
            return Err(TokenError::TooLong);
        }
        Ok(expires)
    }
}

/// The current time in unix seconds (0 if the clock is before 1970).
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    out
}

/// Exactly 32 bytes of lowercase hex (the one spelling `mint` writes), else `None`.
fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() != 64 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    s.as_bytes()
        .chunks(2)
        .map(|pair| Some((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}
