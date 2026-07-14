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
    use super::runs_on_from_base_url;

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
