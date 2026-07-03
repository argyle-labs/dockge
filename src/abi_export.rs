//! ABI-stable cdylib export for the dockge plugin.
//!
//! dockge is a **hybrid** plugin: the `dockge.` endpoint-registry tool surface
//! PLUS one domain backend — a `unit` provider surfacing compose stacks across
//! every registered instance (see [`crate::registration`]). The toolkit's
//! [`export_tool_plugin!`] hybrid arm generates the metadata fns, the
//! `dockge.`-scoped manifest, and an `invoke` that tries the backend dispatch
//! first (the `dockge.__unit.*` calls the loader makes) then falls through to
//! tool dispatch.
//!
//! `abi_stable` remains the crate's one direct non-orca dep because
//! `#[export_root_module]` (which the macro invokes) expands to bare
//! `::abi_stable` paths.

plugin_toolkit::export_tool_plugin! {
    name: "dockge",
    target_compat: "1.x",
    backends: crate::registration::backends_json(),
    backend_dispatch: crate::registration::backend_dispatch,
}
