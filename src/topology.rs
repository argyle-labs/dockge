//! Dockge → TopologyClaim collector.
//!
//! One dockge plugin serves many instances, each managing compose **stacks** on
//! its own host over Socket.IO. This collector emits one claim per stack per
//! enabled endpoint so the fleet inventory records *which dockge host runs which
//! stack* (`provider_instance` = the endpoint name).
//!
//! Unlike the local [`docker`](../../docker) collector, dockge's remote Socket.IO
//! surface exposes **no container network details** — there is no `docker inspect`
//! equivalent, so a stack's MACs are unavailable. Claims therefore carry empty
//! `macs`: they document the endpoint→stack mapping for inventory, but do not
//! themselves drive the MAC-matching parent-nesting inference (that edge comes
//! from the co-located `docker`/`proxmox` collector on the host, or from the
//! dockge endpoint being an orca peer in its own right). Emitting them keeps the
//! stack layer visible in topology rather than silently absent.
//!
//! Enumeration is resilient: an unreachable or failing endpoint is skipped
//! (logged), never fatal to the whole collection — the same policy the unit
//! provider's `all_stacks` uses.
#![allow(clippy::disallowed_types)]

use plugin_toolkit::anyhow::Result;
use plugin_toolkit::contract::TopologyClaim;

use crate::tools::{enabled_endpoint_rows, make_client};

/// Provider name registered in the topology registry.
const PROVIDER: &str = "dockge";
/// Dockge manages compose projects; each stack is a group of containers, but the
/// claim kind mirrors the generic unit vocabulary (`stack`) so it aligns with
/// how the unit surface and the `deploy_target` domain name the same thing.
const KIND: &str = "stack";

/// Enumerate stacks across every enabled dockge endpoint and build one claim per
/// stack. Best-effort per endpoint: a failure to reach or list one instance is
/// logged and skipped, never aborting the rest.
pub async fn collect_claims() -> Result<Vec<TopologyClaim>> {
    let mut claims = Vec::new();
    for row in enabled_endpoint_rows()? {
        let ep = &row.name;
        let client = match make_client(ep) {
            Ok(c) => c,
            Err(e) => {
                plugin_toolkit::tracing::warn!("dockge topology endpoint {ep}: {e}");
                continue;
            }
        };
        // The host this instance (and therefore its stacks) runs on, derived
        // from the endpoint's `base_url`. `None` for a local instance
        // (localhost) — the inventory layer then parents stacks to the
        // reporting peer, which IS the host. For a remote instance the
        // resolved host lets inventory attribute stacks to the right node
        // regardless of which daemon collected them.
        let runs_on = runs_on_from_base_url(&row.base_url);
        match client.list_stacks().await {
            Ok(list) => {
                if let Some(obj) = list.as_object() {
                    for name in obj.keys() {
                        claims.push(TopologyClaim {
                            kind: KIND.to_string(),
                            id: name.clone(),
                            name: name.clone(),
                            // No MACs available over dockge's remote surface.
                            macs: Vec::new(),
                            provider: PROVIDER.to_string(),
                            provider_instance: ep.clone(),
                            runs_on: runs_on.clone(),
                            // Cross-plugin dedup key. dockge manages each stack
                            // as a compose project rooted at `/opt/stacks/<name>`
                            // — the same directory the co-located docker plugin
                            // reads from `com.docker.compose.project.working_dir`.
                            // Routing both providers through the shared
                            // `normalize_service_identity` helper (identical
                            // host scope + working-dir signal) yields a
                            // BYTE-IDENTICAL key, so core collapses the docker
                            // and dockge views onto one stack node.
                            service_identity: stack_service_identity(runs_on.as_deref(), name),
                            // Dockge's remote surface exposes no container
                            // network detail; a dockge host also running the
                            // local docker plugin gets ports via that collector.
                            ..Default::default()
                        });
                    }
                }
            }
            Err(e) => {
                plugin_toolkit::tracing::warn!("dockge topology endpoint {ep} list_stacks: {e}");
            }
        }
    }
    Ok(claims)
}

/// Filesystem root under which dockge lays out every managed compose stack:
/// one directory per project at `/opt/stacks/<project>`. This is the same path
/// the co-located docker plugin observes as `com.docker.compose.project.working_dir`,
/// so building the correlation key from it makes the two providers agree.
const STACKS_ROOT: &str = "/opt/stacks";

/// Build the cross-plugin `service_identity` correlation key for a dockge stack.
///
/// Delegates to [`TopologyClaim::normalize_service_identity`] so the output is
/// byte-identical to what the docker plugin produces for the same stack on the
/// same host: the host scope plus the normalized compose working directory
/// `/opt/stacks/<name>`. The bare `name` is also passed as the project
/// fallback, matching docker's `com.docker.compose.project`.
///
/// Host scope MUST match how docker scopes its own compose claims. Docker
/// stamps the actual hostname via [`plugin_toolkit::containers::local_hostname`]
/// (e.g. `"baldur"`). So for a **co-located** dockge instance (`runs_on` is
/// `None` — a localhost `base_url`, the reporting peer IS the host) we scope by
/// that same `local_hostname()` rather than an empty string; otherwise the two
/// providers on the same physical box would emit divergent keys
/// (`\u{1f}/opt/stacks/<name>` vs `baldur\u{1f}/opt/stacks/<name>`) and core
/// dedup would fail. For a genuinely **remote** instance we keep the resolved
/// remote host from `base_url`, since that box — not this reporter — runs the
/// stack. Returns `None` only if the stack name is blank.
fn stack_service_identity(host: Option<&str>, name: &str) -> Option<String> {
    let working_dir = format!("{STACKS_ROOT}/{name}");
    // Co-located instance: scope by the actual local hostname, the SAME function
    // docker uses, so the keys converge byte-for-byte.
    let host_scope = host.unwrap_or_else(|| plugin_toolkit::containers::local_hostname());
    TopologyClaim::normalize_service_identity(host_scope, Some(&working_dir), Some(name))
}

/// Extract the host segment of a dockge `base_url` for `TopologyClaim.runs_on`.
/// Returns `None` for loopback hosts (the reporting peer is the host) and for
/// unparseable input. The inventory layer matches the returned host against
/// each peer's hostname and network addresses, so an IP or hostname both work.
fn runs_on_from_base_url(base_url: &str) -> Option<String> {
    // Strip scheme.
    let rest = base_url
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(base_url);
    // Authority is everything up to the first '/'.
    let authority = rest.split('/').next().unwrap_or(rest);
    // Drop any userinfo (`user:pass@host`).
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // Drop the port. Bracketed IPv6 (`[::1]:5001`) keeps the brackets' contents.
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next().unwrap_or(stripped)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    let host = host.trim();
    if host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1"
    {
        return None;
    }
    Some(host.to_string())
}

#[cfg(test)]
mod tests {
    use super::{runs_on_from_base_url, stack_service_identity};
    use plugin_toolkit::contract::TopologyClaim;

    #[test]
    fn service_identity_matches_normalized_opt_stacks_form() {
        // The emitted key must equal what docker derives on the same host from
        // `com.docker.compose.project.working_dir = /opt/stacks/<name>`, routed
        // through the shared core helper — byte-for-byte, so core dedups them.
        let got = stack_service_identity(Some("freyr"), "arr");
        let docker_equiv = TopologyClaim::normalize_service_identity(
            "freyr",
            Some("/opt/stacks/arr"),
            Some("arr"),
        );
        assert_eq!(got, docker_equiv);
        assert_eq!(got.as_deref(), Some("freyr\u{1f}/opt/stacks/arr"));
    }

    #[test]
    fn service_identity_colocated_host_uses_local_hostname() {
        // A co-located instance reports `runs_on = None`; the reporting peer IS
        // the host. The key must scope by the actual local hostname — the SAME
        // `plugin_toolkit::containers::local_hostname()` the docker plugin
        // stamps on its own compose claims — so the two providers on the same
        // box emit BYTE-IDENTICAL keys and core dedups them. An empty scope here
        // (the old behavior) forked the key from docker's and broke dedup.
        let host = plugin_toolkit::containers::local_hostname();
        let got = stack_service_identity(None, "media");
        // Docker derives the same key from `local_hostname()` +
        // `working_dir = /opt/stacks/media`, routed through the shared helper.
        let docker_equiv = TopologyClaim::normalize_service_identity(
            host,
            Some("/opt/stacks/media"),
            Some("media"),
        );
        assert_eq!(got, docker_equiv);
        // Host scope is normalized (lowercased) but never empty.
        assert_eq!(
            got.as_deref(),
            Some(format!("{}\u{1f}/opt/stacks/media", host.to_ascii_lowercase()).as_str())
        );
        assert!(!got.as_deref().unwrap().starts_with('\u{1f}'));
    }

    #[test]
    fn loopback_hosts_yield_none() {
        assert_eq!(runs_on_from_base_url("http://localhost:5001"), None);
        assert_eq!(runs_on_from_base_url("http://127.0.0.1:5001"), None);
        assert_eq!(runs_on_from_base_url("http://[::1]:5001"), None);
    }

    #[test]
    fn remote_hostname_and_ip_extracted() {
        assert_eq!(
            runs_on_from_base_url("http://freyr:5001"),
            Some("freyr".to_string())
        );
        assert_eq!(
            runs_on_from_base_url("https://192.0.2.15:5001/"),
            Some("192.0.2.15".to_string())
        );
        assert_eq!(
            runs_on_from_base_url("http://user:pass@baldur:5001"),
            Some("baldur".to_string())
        );
    }
}
