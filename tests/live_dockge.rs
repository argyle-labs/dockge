//! Env-gated live integration test against a real dockge instance.
//!
//! Skipped unless `DOCKGE_TEST_URL` is set (so CI stays green with no live
//! dockge). Point it at any reachable dockge instance:
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
    let obj = stacks.as_object().unwrap();
    eprintln!("live dockge reported {} stack(s):", obj.len());
    for (name, meta) in obj {
        eprintln!("  - {name}: {meta}");
    }

    // Full lifecycle proof (opt-in via DOCKGE_TEST_LIFECYCLE): deploy a
    // throwaway stack dockge fully owns, read it back, restart it (MANAGE),
    // then tear it down + delete it (cleanup). Uses a trivial `alpine` sleeper.
    if env("DOCKGE_TEST_LIFECYCLE").is_some() {
        let name = "orca-validate";
        let compose = "services:\n  probe:\n    image: alpine\n    command: [\"sleep\", \"600\"]\n";

        // Idempotent: clear any leftover from a prior run before deploying.
        client.stack_action(name, "down").await.ok();
        client.delete_stack(name).await.ok();

        let deployed = client
            .deploy_stack(name, compose, "", true)
            .await
            .expect("deploy_stack");
        eprintln!("CREATE deploy {name} -> {deployed}");
        assert_eq!(
            deployed.get("ok").and_then(|v| v.as_bool()),
            Some(true),
            "deploy should succeed: {deployed}"
        );

        let after = client.list_stacks().await.expect("relist");
        assert!(
            after.as_object().is_some_and(|o| o.contains_key(name)),
            "deployed stack should appear in list: {after}"
        );
        eprintln!(
            "READ back {name}: {}",
            after.get(name).map(|v| v.to_string()).unwrap_or_default()
        );

        let managed = client.stack_action(name, "restart").await.expect("restart");
        eprintln!("MANAGE restart {name} -> {managed}");
        assert_eq!(
            managed.get("ok").and_then(|v| v.as_bool()),
            Some(true),
            "restart should succeed: {managed}"
        );

        client.stack_action(name, "down").await.ok();
        let deleted = client.delete_stack(name).await.expect("delete");
        eprintln!("CLEANUP delete {name} -> {deleted}");
    }
}
