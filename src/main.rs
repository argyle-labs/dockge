//! Dynamic (subprocess) entrypoint for the dockge plugin.
//!
//! The toolkit's `serve_tool_plugin!` emits `fn main`, serving this plugin over
//! the orca socket. Dynamic replacement for the retired cdylib export — the
//! plugin is a `[[bin]]`, owns no runtime, and reaches orca only through the
//! socket.
//!
//! dockge is a **hybrid** plugin: the `dockge.` endpoint-registry tool surface
//! PLUS one domain backend — a `unit` provider surfacing compose stacks across
//! every registered instance (see [`dockge::registration`]). The hybrid arm
//! serves the `dockge.`-scoped manifest and an `invoke` that tries the backend
//! dispatch first (the `dockge.__unit.*` calls the loader makes) then falls
//! through to tool dispatch.

plugin_toolkit::serve_tool_plugin! {
    name: "dockge",
    target_compat: "1.x",
    backends: dockge::registration::backends_json(),
    backend_dispatch: dockge::registration::backend_dispatch,
}
