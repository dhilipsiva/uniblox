//! The signaling binary is secure by default (ADR-0047): without a signaling secret it refuses to
//! start unless told it is a development server, and `mint-token` prints a token the gate accepts.

use std::process::Command;

use services::{JoinTokenKey, unix_now};

const SECRET: &str = "0123456789abcdef0123456789abcdef";

fn services() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_services"));
    cmd.env_remove("UNIBLOX_SIGNALING_SECRET")
        .env_remove("UNIBLOX_SIGNALING_INSECURE")
        .env_remove("UNIBLOX_REDIS_URL")
        .env("UNIBLOX_SIGNALING_PORT", "0");
    cmd
}

#[test]
fn the_binary_refuses_to_start_without_a_secret() {
    let out = services().output().expect("run the binary");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("UNIBLOX_SIGNALING_SECRET"), "{stderr}");
}

#[test]
fn a_short_secret_is_refused() {
    let out = services()
        .env("UNIBLOX_SIGNALING_SECRET", "short")
        .output()
        .expect("run the binary");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn mint_token_prints_a_token_for_the_room() {
    let room = "m3~1.0~0~arena";
    let out = services()
        .env("UNIBLOX_SIGNALING_SECRET", SECRET)
        .args(["mint-token", room, "120"])
        .output()
        .expect("run the binary");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let token = String::from_utf8(out.stdout).unwrap();
    let key = JoinTokenKey::new(SECRET.as_bytes()).unwrap();
    assert_eq!(key.verify(token.trim(), room, unix_now()), Ok(()));
    let without = services()
        .args(["mint-token", room])
        .output()
        .expect("run the binary");
    assert_eq!(without.status.code(), Some(2), "minting needs the secret");
}

/// `mint-token` refuses what the gate would refuse: an over-long lifetime (or one that overflows)
/// and a room the gate rejects.
#[test]
fn mint_token_refuses_what_the_gate_refuses() {
    for args in [
        vec!["mint-token", "m3~1.0~0~arena", "18446744073709551615"],
        vec!["mint-token", "m3~1.0~0~arena", "604800"],
        vec!["mint-token", "a#0"],
        vec!["mint-token", ""],
        vec!["mint-token", "m3~bad"],
    ] {
        let out = services()
            .env("UNIBLOX_SIGNALING_SECRET", SECRET)
            .args(&args)
            .output()
            .expect("run the binary");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}

/// The secret is taken as raw bytes, UTF-8 or not.
#[cfg(unix)]
#[test]
fn a_non_utf8_secret_is_used_as_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let raw: Vec<u8> = (0u8..40).map(|b| b ^ 0xA5).collect();
    let out = services()
        .env(
            "UNIBLOX_SIGNALING_SECRET",
            std::ffi::OsStr::from_bytes(&raw),
        )
        .args(["mint-token", "room"])
        .output()
        .expect("run the binary");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let token = String::from_utf8(out.stdout).unwrap();
    let key = JoinTokenKey::new(&raw).unwrap();
    assert_eq!(key.verify(token.trim(), "room", unix_now()), Ok(()));
}

/// Asking for the token endpoint without a secret is a configuration error, not a silent no-op.
#[test]
fn issuing_tokens_needs_a_secret() {
    let out = services()
        .env("UNIBLOX_SIGNALING_INSECURE", "1")
        .env("UNIBLOX_ISSUE_TOKENS", "1")
        .output()
        .expect("run the binary");
    assert_eq!(out.status.code(), Some(2));
}

/// TURN behind the open token endpoint would be an open relay: refused unless the operator says so.
#[test]
fn turn_with_open_tokens_needs_an_explicit_opt_in() {
    let out = services()
        .env("UNIBLOX_SIGNALING_SECRET", SECRET)
        .env("UNIBLOX_ISSUE_TOKENS", "1")
        .env(
            "UNIBLOX_TURN_SECRET",
            "0123456789abcdef0123456789abcdef-turn",
        )
        .env("UNIBLOX_TURN_URLS", "turn:127.0.0.1:3478")
        .output()
        .expect("run the binary");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("UNIBLOX_TURN_FOR_OPEN_TOKENS"), "{stderr}");
}

/// A weak TURN secret or a URL that is not a TURN or STUN URL is a configuration error.
#[test]
fn turn_config_is_checked() {
    for (secret, urls) in [
        ("short", "turn:127.0.0.1:3478"),
        (
            "0123456789abcdef0123456789abcdef-turn",
            "http://example.com",
        ),
    ] {
        let out = services()
            .env("UNIBLOX_SIGNALING_SECRET", SECRET)
            .env("UNIBLOX_TURN_SECRET", secret)
            .env("UNIBLOX_TURN_URLS", urls)
            .output()
            .expect("run the binary");
        assert_eq!(out.status.code(), Some(2), "{secret} {urls}");
    }
}
