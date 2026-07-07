//! Domain-backend registration for the hybrid export.
//!
//! dockge contributes two backends to orca's `contract` registries, both routed
//! back through the FFI `invoke` under a distinct prefix:
//!
//! - `unit` (`dockge.__unit.<op>`) — compose **stacks** across every registered
//!   dockge instance, surfaced on the generic five-verb + `action` surface (see
//!   [`crate::unit_provider`]).
//! - `topology` (`dockge.__topo.collect_claims`) — one [`TopologyClaim`] per
//!   stack per enabled endpoint, so the fleet inventory records which dockge host
//!   runs which stack (see [`crate::topology`]).
//!
//! `service` and `container_runtime`/deploy-target registrations remain
//! follow-ups (each dockge instance as a deploy target for stacks). Those need
//! dynamic per-endpoint (re)registration as instances are added/removed, so they
//! land separately from this static-descriptor surface.
//!
//! [`backend_dispatch`] answers `dockge.__unit.*` / `dockge.__topo.*`; the
//! toolkit's hybrid `invoke` routes everything else to the `dockge.`
//! endpoint-registry surface.

use std::sync::OnceLock;

use plugin_toolkit::abi::BackendDef;
use plugin_toolkit::contract::unit::UnitProvider;
use plugin_toolkit::export::{dispatch_unit_op, runtime, topology_backend_def, unit_backend_def};
use plugin_toolkit::serde_json;

use crate::unit_provider::DockgeUnitProvider;

const UNIT_PREFIX: &str = "dockge.__unit";
const TOPO_PREFIX: &str = "dockge.__topo";

fn unit_provider() -> &'static DockgeUnitProvider {
    static PROVIDER: OnceLock<DockgeUnitProvider> = OnceLock::new();
    PROVIDER.get_or_init(DockgeUnitProvider::new)
}

/// Backend descriptors this plugin advertises: the unit provider surfacing dockge
/// compose stacks and a topology collector, each routed back under its own
/// prefix. Both descriptors are derived from the live surface via the toolkit's
/// export helpers so the registered kinds/verbs stay in sync automatically.
pub fn backends_json() -> String {
    let defs: Vec<BackendDef> = vec![
        unit_backend_def(unit_provider() as &dyn UnitProvider, UNIT_PREFIX),
        topology_backend_def("dockge", TOPO_PREFIX),
    ];
    serde_json::to_string(&defs).unwrap_or_else(|_| "[]".to_string())
}

/// Handle the loader's `dockge.__unit.*` / `dockge.__topo.*` backend calls.
/// Returns `None` for anything else so the toolkit falls through to the
/// `dockge.` tool surface. Async work runs on the toolkit's shared runtime
/// behind the synchronous FFI boundary.
pub fn backend_dispatch(name: &str, args_json: &str) -> Option<Result<String, String>> {
    if let Some(op) = name
        .strip_prefix(UNIT_PREFIX)
        .and_then(|s| s.strip_prefix('.'))
    {
        return Some(dispatch_unit_op(
            unit_provider() as &dyn UnitProvider,
            op,
            args_json,
        ));
    }
    if let Some(op) = name
        .strip_prefix(TOPO_PREFIX)
        .and_then(|s| s.strip_prefix('.'))
    {
        return Some(dispatch_topology(op));
    }
    None
}

fn dispatch_topology(op: &str) -> Result<String, String> {
    match op {
        "collect_claims" => {
            let claims = runtime()
                .block_on(crate::topology::collect_claims())
                .map_err(|e| e.to_string())?;
            serde_json::to_string(&claims).map_err(|e| e.to_string())
        }
        other => Err(format!("unknown topology op: {other}")),
    }
}
