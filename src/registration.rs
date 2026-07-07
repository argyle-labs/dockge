//! Domain-backend registration for the hybrid export.
//!
//! dockge contributes a `unit` backend to orca's `contract` registry: compose
//! **stacks** across every registered dockge instance, surfaced on the generic
//! five-verb + `action` surface (see [`crate::unit_provider`]). It is routed
//! back through the FFI `invoke` under the `dockge.__unit` prefix.
//!
//! `service`, `container_runtime`/deploy-target, and `topology` registrations
//! are follow-ups (each dockge instance as a deploy target for stacks; a
//! topology collector for fleet nesting). This PR lands the unit surface — the
//! minimum to replace the removed bespoke stack tools.
//!
//! [`backend_dispatch`] answers `dockge.__unit.*`; the toolkit's hybrid
//! `invoke` routes everything else to the `dockge.` endpoint-registry surface.

use std::sync::OnceLock;

use plugin_toolkit::contract::unit::UnitProvider;
use plugin_toolkit::export::{dispatch_unit_op, unit_backends_json};

use crate::unit_provider::DockgeUnitProvider;

const UNIT_PREFIX: &str = "dockge.__unit";

fn unit_provider() -> &'static DockgeUnitProvider {
    static PROVIDER: OnceLock<DockgeUnitProvider> = OnceLock::new();
    PROVIDER.get_or_init(DockgeUnitProvider::new)
}

/// Backend descriptors this plugin advertises: a unit provider surfacing dockge
/// compose stacks, routed back under its own prefix.
pub fn backends_json() -> String {
    unit_backends_json(unit_provider() as &dyn UnitProvider, UNIT_PREFIX)
}

/// Handle the loader's `dockge.__unit.*` backend calls. Returns `None` for
/// anything else so the toolkit falls through to the `dockge.` tool surface.
/// Async work runs on the toolkit's shared runtime behind the synchronous FFI
/// boundary.
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
    None
}
