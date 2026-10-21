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
