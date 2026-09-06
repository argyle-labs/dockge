//! Dynamic (subprocess) entrypoint for the dockge plugin.
//!
//! dockge is a **hybrid** plugin: the `dockge.` endpoint-registry `#[orca_tool]`
//! surface PLUS two typed domain backends — a `unit` provider (compose stacks
//! across every registered instance) and a `topology` collector (one claim per
//! stack per endpoint). All three are registered on the [`Plugin`] builder, which
//! emits the combined `backends()` payload and the wire dispatch. The plugin
//! hand-writes no op-string routing.

plugin_toolkit::instrument::bootstrap!();

use plugin_toolkit::plugin::Plugin;

// Force-link the `dockge.` #[orca_tool] surface so its inventory (a separate
// module from the backends referenced below) isn't dead-stripped at link time.
use dockge as _;

fn main() -> plugin_toolkit::anyhow::Result<()> {
    Plugin::named("dockge")
        .version(env!("CARGO_PKG_VERSION"))
        .tools(["dockge."])
        .unit(dockge::unit_provider::DockgeUnitProvider::new())
        .topology(dockge::topology::DockgeTopology)
        .serve()
}
