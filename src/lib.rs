//! Dockge stack-manager plugin. Socket.IO client + domain registrations.
//!
//! Dockge exposes **no REST API** — it is Socket.IO v4 (Engine.IO v4) over
//! WebSocket, auth = username/password → JWT in the login ack, authorized
//! per-connection. This [`Client`] wraps the toolkit's generic
//! [`plugin_toolkit::socketio`] transport; the plugin owns only dockge's event
//! vocabulary.
//!
//! Surface: dockge registers real domain backends (see [`registration`]) —
//! `unit` (compose stacks as managed units on the five-verb surface), plus the
//! endpoint registry `dockge.{list,detail,create,update,delete}` (the dockge
//! *instances*). No bespoke **stack** verbs — stacks ride the generic unit
//! surface.
//!
//! Deploy/backup/restore of the dockge web app *itself* lives in `scripts/`
//! (standalone) and returns as a proper `service` domain backend in a
//! follow-up — it is deliberately NOT a set of bespoke `dockge.*` tool verbs
//! (the old `lifecycle` module both collided with the endpoint registry's
//! `dockge.update` and violated the five-verb surface).
// Socket.IO frames are dynamic JSON; Value lives only at this transport edge.
#![allow(clippy::disallowed_types)]

pub mod abi_export;
pub mod registration;
pub mod tools;
pub mod unit_provider;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use plugin_toolkit::anyhow::{Context, Result, bail};
use plugin_toolkit::serde_json::{Value, json};
use plugin_toolkit::socketio::{PushHandler, SocketConfig, SocketSession};

const ACK_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Deploying pulls images + starts containers — allow much longer than a
/// plain ack.
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(120);

/// Dockge routes every stack op through an **agent wrapper**: the client emits
/// the real event inside an `"agent"` frame — `emit("agent", endpoint, event,
/// …args)` — and the server pushes back as `["<event>", <data>]`. For a direct
/// connection the endpoint is empty (dockge treats a falsy endpoint as "this
/// instance"). Raw events (`requestStackList`, `getStack`, …) are silently
/// ignored by the server, so they MUST go through this wrapper.
const AGENT_LOCAL: &str = "";

/// Build the positional args for an agent-wrapped emit: `[endpoint, event,
/// …extra]`.
fn agent_args(event: &str, extra: &[Value]) -> Vec<Value> {
    let mut args = vec![json!(AGENT_LOCAL), json!(event)];
    args.extend_from_slice(extra);
    args
}

/// Connection details for one dockge instance. `password` is supplied by the
/// caller (loaded from the endpoint row's `#[secret]` field / the secrets
/// domain); it is never logged.
#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: String,
    pub username: String,
    pub password: String,
    /// Accept self-signed TLS (homelab `wss`).
    pub insecure: bool,
}

impl Config {
    pub fn new(
        base_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            username: username.into(),
            password: password.into(),
            insecure: false,
        }
    }

    pub fn insecure(mut self, on: bool) -> Self {
        self.insecure = on;
        self
    }
}

/// A dockge client. Each call opens a short-lived authenticated Socket.IO
/// session (Socket.IO auth is per-connection); a shared session cache is a
/// future toolkit optimisation, not required for correctness.
#[derive(Clone)]
pub struct Client {
    cfg: Config,
}

impl Client {
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// Connect and authenticate. `stacklist_slot`, if set, captures dockge's
    /// pushed `stackList` (streamed on connect + on change) out of the `agent`
    /// frame it arrives in.
    async fn session(
        &self,
        stacklist_slot: Option<Arc<Mutex<Option<Value>>>>,
    ) -> Result<SocketSession> {
        let mut handlers: Vec<(String, PushHandler)> = Vec::new();
        if let Some(slot) = stacklist_slot {
            handlers.push((
                "agent".to_string(),
                Arc::new(move |v: Value| {
                    // Server → client agent frame: `["stackList", { ok,
                    // stackList, endpoint }]`. Capture the data object.
                    if let Some(arr) = v.as_array()
                        && arr.first().and_then(Value::as_str) == Some("stackList")
                        && let Ok(mut g) = slot.lock()
                    {
                        *g = Some(arr.get(1).cloned().unwrap_or(Value::Null));
                    }
                }) as PushHandler,
            ));
        }

        let session = SocketSession::connect_with(
            SocketConfig::new(&self.cfg.base_url)
                .insecure(self.cfg.insecure)
                .connect_timeout(CONNECT_TIMEOUT),
            handlers,
        )
        .await
        .with_context(|| format!("connect dockge at {}", self.cfg.base_url))?;

        // Dockge login: username/password (+ empty 2FA token) → JWT in the ack.
        let ack = session
            .emit_ack(
                "login",
                json!({
                    "username": self.cfg.username,
                    "password": self.cfg.password,
                    "token": "",
                }),
                ACK_TIMEOUT,
            )
            .await
            .context("dockge login")?;
        if !ack_ok(&ack) {
            bail!("dockge login rejected: {}", ack_msg(&ack));
        }
        Ok(session)
    }

    /// All stacks on this instance, as dockge's `stackList` map
    /// (`{ name: { status, tags, … } }`).
    pub async fn list_stacks(&self) -> Result<Value> {
        let slot = Arc::new(Mutex::new(None));
        let session = self.session(Some(slot.clone())).await?;
        // Agent-wrapped request; dockge replies by pushing the `stackList`
        // agent frame (no ack), which the session handler captures.
        session
            .emit_args("agent", agent_args("requestStackList", &[]))
            .await
            .ok();
        let payload = wait_for_slot(&slot).await.unwrap_or_else(|| json!({}));
        session.disconnect().await.ok();
        // payload = `{ ok, stackList: { name: {…} }, endpoint }`.
        Ok(payload
            .get("stackList")
            .cloned()
            .unwrap_or_else(|| json!({})))
    }

    /// One stack's detail (`{ name, composeYAML, composeENV, status, … }`).
    pub async fn get_stack(&self, name: &str) -> Result<Value> {
        if name.is_empty() {
            bail!("missing stack name");
        }
        let session = self.session(None).await?;
        let ack = session
            .emit_ack_args("agent", agent_args("getStack", &[json!(name)]), ACK_TIMEOUT)
            .await;
        session.disconnect().await.ok();
        ack
    }

    /// Run a lifecycle action against a stack. `op` ∈
    /// `start` | `stop` | `restart` | `down` | `update` — mapped to dockge's
    /// agent-wrapped `<op>Stack` event.
    pub async fn stack_action(&self, name: &str, op: &str) -> Result<Value> {
        if name.is_empty() {
            bail!("missing stack name");
        }
        let event = match op {
            "start" | "stop" | "restart" | "down" | "update" => format!("{op}Stack"),
            other => bail!("unknown stack action: {other}"),
        };
        let session = self.session(None).await?;
        // Lifecycle actions shell out to `docker compose` (start/stop/pull) and
        // can take far longer than a plain ack, so allow the deploy timeout.
        let ack = session
            .emit_ack_args("agent", agent_args(&event, &[json!(name)]), DEPLOY_TIMEOUT)
            .await;
        session.disconnect().await.ok();
        ack
    }

    /// Remove a stack.
    pub async fn delete_stack(&self, name: &str) -> Result<Value> {
        if name.is_empty() {
            bail!("missing stack name");
        }
        let session = self.session(None).await?;
        let ack = session
            .emit_ack_args(
                "agent",
                agent_args("deleteStack", &[json!(name)]),
                ACK_TIMEOUT,
            )
            .await;
        session.disconnect().await.ok();
        ack
    }

    /// Create + deploy a stack (dockge `deployStack`: `docker compose up -d`).
    /// `is_add` = true for a new stack. Deploying pulls images + starts
    /// containers, so this uses a longer timeout than the ack ops.
    pub async fn deploy_stack(
        &self,
        name: &str,
        compose_yaml: &str,
        compose_env: &str,
        is_add: bool,
    ) -> Result<Value> {
        if name.is_empty() {
            bail!("missing stack name");
        }
        let session = self.session(None).await?;
        let ack = session
            .emit_ack_args(
                "agent",
                agent_args(
                    "deployStack",
                    &[
                        json!(name),
                        json!(compose_yaml),
                        json!(compose_env),
                        json!(is_add),
                    ],
                ),
                DEPLOY_TIMEOUT,
            )
            .await;
        session.disconnect().await.ok();
        ack
    }
}

/// A dockge ack is `{ ok: bool, msg?: string, … }`.
fn ack_ok(ack: &Value) -> bool {
    ack.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

fn ack_msg(ack: &Value) -> String {
    ack.get("msg")
        .and_then(Value::as_str)
        .unwrap_or("no message")
        .to_string()
}

/// Poll a capture slot for a pushed payload, up to ~2s.
async fn wait_for_slot(slot: &Arc<Mutex<Option<Value>>>) -> Option<Value> {
    for _ in 0..20 {
        if let Ok(mut g) = slot.lock()
            && let Some(v) = g.take()
        {
            return Some(v);
        }
        plugin_toolkit::tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_ok_reads_bool() {
        assert!(ack_ok(&json!({ "ok": true })));
        assert!(!ack_ok(&json!({ "ok": false })));
        assert!(!ack_ok(&json!({})));
    }

    #[test]
    fn ack_msg_falls_back() {
        assert_eq!(ack_msg(&json!({ "msg": "bad login" })), "bad login");
        assert_eq!(ack_msg(&json!({})), "no message");
    }

    #[test]
    fn config_hides_password_construction() {
        let c = Config::new("wss://dockge.lan:5001", "svc", "pw").insecure(true);
        assert_eq!(c.username, "svc");
        assert!(c.insecure);
    }
}
