//! Env-gated live integration test against a real dockge instance.
//!
//! Skipped unless `DOCKGE_TEST_URL` is set (so CI stays green with no live
//! dockge). Point it at one of the fleet instances (baldur/freyr/willow):
//!
//! ```sh
//! DOCKGE_TEST_URL=wss://dockge.example:5001 \
//! DOCKGE_TEST_USER=admin \
//! DOCKGE_TEST_PASS=… \
//! DOCKGE_TEST_INSECURE=1 \
//!   cargo test -p dockge --test live_dockge -- --nocapture
//! ```
//!
//! Never commit real addresses/creds — they flow only through these env vars.

use dockge::{Client, Config};

fn env(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

#[tokio::test]
async fn lists_stacks_against_live_dockge() {
    let Some(url) = env("DOCKGE_TEST_URL") else {
        eprintln!("skipping: DOCKGE_TEST_URL unset");
        return;
    };
    let user = env("DOCKGE_TEST_USER").unwrap_or_default();
    let pass = env("DOCKGE_TEST_PASS").unwrap_or_default();
    let insecure = env("DOCKGE_TEST_INSECURE").is_some();

    let client = Client::new(Config::new(url, user, pass).insecure(insecure));
    let stacks = client
        .list_stacks()
        .await
        .expect("list_stacks against live dockge");

    assert!(
        stacks.is_object(),
        "expected a stackList map, got: {stacks}"
    );
    eprintln!(
        "live dockge reported {} stack(s)",
        stacks.as_object().map(|o| o.len()).unwrap_or(0)
    );
}
