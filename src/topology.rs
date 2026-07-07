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

use crate::tools::{enabled_endpoints, make_client};

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
    for ep in enabled_endpoints()? {
        let client = match make_client(&ep) {
            Ok(c) => c,
            Err(e) => {
                plugin_toolkit::tracing::warn!("dockge topology endpoint {ep}: {e}");
                continue;
            }
        };
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
