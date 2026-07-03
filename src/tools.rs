//! Dockge endpoint registry: `dockge.{list, detail, create, update, delete}` —
//! the dockge **instances**, generated wholesale by `endpoint_resource!`. The
//! macro emits the row struct, db helpers (`endpoint_db::{list,get,insert,
//! update,upsert,remove}`), schema fragment, args/output types, and the five
//! `#[orca_tool]` functions in one shot.
//!
//! Compose **stacks are NOT tools here** — they are surfaced as units through
//! [`crate::unit_provider`] (the generic five-verb + `action` surface), so the
//! plugin adds no bespoke `stacks` / `stack_action` / `stack_logs` verbs. The
//! socketio [`Client`] is consumed internally by the domain adapter, never by
//! hand-written stack tools.
//!
//! Each instance stores `base_url` + `username` + a `#[secret] password`; the
//! secret is excluded from list/detail output and loaded only when building a
//! client.
//!
//! Imports flow through `plugin_toolkit::prelude::*` only — the plugin treats
//! the toolkit as the single gateway to the orca system.
#![allow(clippy::disallowed_types)]

use plugin_toolkit::prelude::*;

use crate::{Client, Config};

// ═══════════════════════════════════════════════════════════════════════════
// dockge.{list,detail,create,update,delete} — endpoint registry CRUD.
// One declaration → five tools, three transports each, schema fragment, db
// helpers, row struct, args/output types.
// ═══════════════════════════════════════════════════════════════════════════

#[endpoint_resource(plugin = "dockge")]
pub struct DockgeEndpoint {
    pub name: String,
    pub base_url: String,
    pub username: String,
    #[secret]
    pub password: String,
    pub enabled: bool,
}

/// Build a client for a registered endpoint by name. Used by the unit provider
/// to drive that instance's stacks over Socket.IO.
pub(crate) fn make_client(name: &str) -> Result<Client> {
    let conn = runtime::open_db()?;
    let row = endpoint_db::get(&conn, name)?
        .with_context(|| format!("dockge endpoint '{name}' not registered"))?;
    if !row.enabled {
        bail!("dockge endpoint '{name}' is disabled");
    }
    Ok(Client::new(Config::new(
        row.base_url,
        row.username,
        row.password,
    )))
}

/// Names of all registered, enabled dockge endpoints.
pub(crate) fn enabled_endpoints() -> Result<Vec<String>> {
    let conn = runtime::open_db()?;
    Ok(endpoint_db::list(&conn)?
        .into_iter()
        .filter(|r| r.enabled)
        .map(|r| r.name)
        .collect())
}
