//! Dockge tool surface.
//!
//! Endpoint registry: `dockge.{list, detail, create, update, delete}` —
//! generated wholesale by `endpoint_resource!`. The macro emits the row
//! struct, db helpers, schema fragment, args/output types, and the five
//! `#[orca_tool]`-annotated functions in one shot. See
//! [[feedback-plugin-toolkit-max-power-min-boilerplate]].
//!
//! Stack ops: a stack is a distinct resource managed *through* an
//! endpoint, not a verb on the endpoint. `dockge.stacks` /
//! `dockge.stack_logs` / `dockge.stack_action` cover the upstream Dockge
//! API and are hand-written since they call out over HTTP rather than
//! over the local registry table.
//!
//! Endpoint resolution: stack-op tools accept the endpoint *name* and
//! load `(base_url, token)` from the toolkit-generated `endpoint_db` at
//! call time. Per [[project-colocated-api-clients]] + model B (any
//! creds-holder may execute), the row syncs to every paired peer so any
//! of them can call `dockge.*` against a registered endpoint.
//!
//! Imports flow through `plugin_toolkit::prelude::*` only — the
//! plugin treats the toolkit as the single gateway to the orca system.
#![allow(clippy::disallowed_types)]

use plugin_toolkit::prelude::*;

use crate::{Client, Config};

// ═══════════════════════════════════════════════════════════════════════════
// dockge.{list,detail,create,update,delete} — endpoint registry CRUD.
// One declaration → five tools, three transports each, schema fragment, db
// helpers, row struct, args/output types. Power scales with the macro.
// ═══════════════════════════════════════════════════════════════════════════

#[endpoint_resource(plugin = "dockge")]
pub struct DockgeEndpoint {
    pub name: String,
    pub base_url: String,
    #[secret]
    pub token: String,
    pub enabled: bool,
}

// ── HTTP client helper ──────────────────────────────────────────────────────

fn make_client(name: &str) -> Result<Client> {
    let conn = runtime::open_db()?;
    let row = endpoint_db::get(&conn, name)?
        .with_context(|| format!("dockge endpoint '{name}' not registered"))?;
    if !row.enabled {
        bail!("dockge endpoint '{name}' is disabled");
    }
    Ok(Client::new(Config::new(row.base_url, row.token)))
}

// ═══════════════════════════════════════════════════════════════════════════
// dockge.stacks — list stacks managed by one endpoint
// ═══════════════════════════════════════════════════════════════════════════

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct DockgeStacksArgs {
    /// Registered endpoint name.
    pub endpoint: String,
}

#[derive(
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct DockgeStacksOutput {
    pub stacks: JsonAny,
}

/// List stacks managed by a registered Dockge endpoint.
#[orca_tool(domain = "dockge", verb = "stacks")]
async fn dockge_stacks(args: DockgeStacksArgs, _ctx: &ToolCtx) -> Result<DockgeStacksOutput> {
    let client = make_client(&args.endpoint)?;
    Ok(DockgeStacksOutput {
        stacks: client.list().await?.into(),
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// dockge.stack_logs — recent logs for one stack
// ═══════════════════════════════════════════════════════════════════════════

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct DockgeStackLogsArgs {
    pub endpoint: String,
    /// Stack name (e.g. "sonarr").
    pub stack: String,
}

/// Fetch recent logs for a single Dockge stack.
#[orca_tool(domain = "dockge", verb = "stack_logs")]
async fn dockge_stack_logs(args: DockgeStackLogsArgs, _ctx: &ToolCtx) -> Result<JsonAny> {
    let client = make_client(&args.endpoint)?;
    Ok(client.logs(&args.stack).await?.into())
}

// ═══════════════════════════════════════════════════════════════════════════
// dockge.stack_action — start / stop / restart one stack
// ═══════════════════════════════════════════════════════════════════════════

/// Stack lifecycle action. Anything other than these three is rejected —
/// Dockge itself only supports these. Named `StackOp` (not
/// `DockgeStackAction`) because `#[orca_tool]` derives a `DockgeStackAction`
/// ZST from the `dockge_stack_action` fn name, and the two would collide.
#[derive(
    plugin_toolkit::clap::ValueEnum,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
    Clone,
    Copy,
    Debug,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "lowercase")]
pub enum StackOp {
    Start,
    Stop,
    Restart,
}

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct DockgeStackActionArgs {
    pub endpoint: String,
    pub stack: String,
    #[arg(value_enum)]
    pub action: StackOp,
}

#[derive(
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct DockgeStackActionOutput {
    pub endpoint: String,
    pub stack: String,
    pub action: StackOp,
    pub status: u16,
}

/// [MUTATES STATE] Run a lifecycle action on a Dockge stack.
#[orca_tool(domain = "dockge", verb = "stack_action")]
async fn dockge_stack_action(
    args: DockgeStackActionArgs,
    _ctx: &ToolCtx,
) -> Result<DockgeStackActionOutput> {
    let client = make_client(&args.endpoint)?;
    let result = match args.action {
        StackOp::Start => client.start(&args.stack).await?,
        StackOp::Stop => client.stop(&args.stack).await?,
        StackOp::Restart => client.restart(&args.stack).await?,
    };
    Ok(DockgeStackActionOutput {
        endpoint: args.endpoint,
        stack: args.stack,
        action: args.action,
        status: result.status,
    })
}
