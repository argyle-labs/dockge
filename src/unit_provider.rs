//! Dockge [`UnitProvider`] — compose **stacks** across every registered dockge
//! instance, surfaced as units on the generic five-verb + `action` surface.
//!
//! One dockge plugin serves many instances, so a stack's [`UnitId::manager`] is
//! `dockge@<endpoint>` — `invoke` parses the endpoint out of it and drives the
//! right instance over Socket.IO. Each stack is a `stack` kind unit. Verbs map:
//! - [`Verb::List`]   → every stack on every enabled endpoint
//! - [`Verb::Detail`] → one stack's compose YAML / env / status
//! - [`Verb::Update`] → action `start` / `stop` / `restart` / `down` / `update`
//! - [`Verb::Delete`] → remove the stack
//! - [`Verb::Create`] → action `deploy`: `deployStack` a new stack (add-only)
//! - [`Verb::Upsert`] → action `set`: deploy add-if-absent, else update
//!
//! Enumeration is resilient: an unreachable or failing endpoint is skipped
//! (logged), never fatal to the whole list.
#![allow(clippy::disallowed_types)]

use plugin_toolkit::anyhow::{self, Result};
use plugin_toolkit::contract::BoxFuture;
use plugin_toolkit::contract::unit::{
    ActionDecl, ActionOutcome, CreateArgs, DeleteArgs, DetailArgs, ItemOutcome, ItemsOutcome,
    KindDeclaration, ListArgs, UnitDescriptor, UnitId, UnitProvider, UpdateArgs, UpsertArgs, Verb,
    VerbArgs, VerbDecl, VerbOutcome,
};
use plugin_toolkit::schemars::{JsonSchema, schema_for};
use plugin_toolkit::serde::{Deserialize, Serialize};
use plugin_toolkit::serde_json::{self, Value, json};

use crate::tools::{enabled_endpoints, make_client};

const KIND: &str = "stack";

/// Typed payload for `Create { action: "deploy" }` and `Upsert { action: "set" }`
/// — everything dockge's `deployStack` (`docker compose up -d`) needs. `create`
/// always adds a fresh stack; `upsert` decides add-vs-update from whether the
/// named stack already exists on the endpoint.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct StackDeployPayload {
    /// Registered dockge endpoint (instance name) to deploy on.
    pub endpoint: String,
    /// Stack name — the compose project directory dockge creates.
    pub name: String,
    /// The `docker-compose.yaml` document.
    pub compose_yaml: String,
    /// Optional `.env` contents for the stack (default: empty).
    #[serde(default)]
    pub compose_env: String,
}

/// Lifecycle actions accepted on `Verb::Update` — each maps to dockge's
/// single-arg `<op>Stack` event.
const ACTIONS: &[&str] = &["start", "stop", "restart", "down", "update"];

#[derive(Default)]
pub struct DockgeUnitProvider;

impl DockgeUnitProvider {
    pub fn new() -> Self {
        Self
    }

    fn manager(endpoint: &str) -> String {
        format!("dockge@{endpoint}")
    }

    fn endpoint_of(manager: &str) -> &str {
        manager.strip_prefix("dockge@").unwrap_or(manager)
    }

    fn unit_id(endpoint: &str, stack: &str) -> UnitId {
        UnitId {
            manager: Self::manager(endpoint),
            kind: KIND.into(),
            id: stack.to_string(),
            name: stack.to_string(),
        }
    }

    /// Collect `(endpoint, stack_name, stack_meta)` across every enabled
    /// endpoint, skipping any instance that can't be reached or listed.
    async fn all_stacks() -> Result<Vec<(String, String, Value)>> {
        let mut out = Vec::new();
        for ep in enabled_endpoints()? {
            let client = match make_client(&ep) {
                Ok(c) => c,
                Err(e) => {
                    plugin_toolkit::tracing::warn!("dockge endpoint {ep}: {e}");
                    continue;
                }
            };
            match client.list_stacks().await {
                Ok(list) => {
                    if let Some(obj) = list.as_object() {
                        for (name, meta) in obj {
                            out.push((ep.clone(), name.clone(), meta.clone()));
                        }
                    }
                }
                Err(e) => {
                    plugin_toolkit::tracing::warn!("dockge endpoint {ep} list_stacks: {e}");
                }
            }
        }
        Ok(out)
    }

    async fn do_list(&self, _args: ListArgs) -> Result<VerbOutcome> {
        let stacks = Self::all_stacks().await?;
        let items = stacks
            .into_iter()
            .map(|(ep, name, meta)| {
                ItemOutcome::new(
                    Self::unit_id(&ep, &name),
                    serde_json::to_string(&json!({
                        "endpoint": ep,
                        "stack": name,
                        "meta": meta,
                    }))
                    .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        let total = items.len() as u64;
        Ok(VerbOutcome::Items(ItemsOutcome {
            items,
            total: Some(total),
        }))
    }

    async fn do_detail(&self, args: DetailArgs) -> Result<VerbOutcome> {
        let ep = Self::endpoint_of(&args.id.manager).to_string();
        let client = make_client(&ep)?;
        let stack = client.get_stack(&args.id.id).await?;
        Ok(VerbOutcome::Item(ItemOutcome::new(
            args.id,
            serde_json::to_string(&stack).unwrap_or_default(),
        )))
    }

    async fn do_update(&self, args: UpdateArgs) -> Result<VerbOutcome> {
        let ep = Self::endpoint_of(&args.id.manager).to_string();
        let op = args.action.as_str();
        if !ACTIONS.contains(&op) {
            return Err(anyhow::anyhow!("unknown stack update action: {op}"));
        }
        let client = make_client(&ep)?;
        client.stack_action(&args.id.id, op).await?;
        Ok(VerbOutcome::Action(ActionOutcome {
            changed: true,
            message: format!("{op} {} on {ep}", args.id.id),
        }))
    }

    async fn do_delete(&self, args: DeleteArgs) -> Result<VerbOutcome> {
        let ep = Self::endpoint_of(&args.id.manager).to_string();
        let client = make_client(&ep)?;
        client.delete_stack(&args.id.id).await?;
        Ok(VerbOutcome::Action(ActionOutcome {
            changed: true,
            message: format!("deleted {} on {ep}", args.id.id),
        }))
    }

    /// Parse the shared deploy payload from a create/upsert args string.
    fn parse_deploy_payload(raw: Option<String>) -> Result<StackDeployPayload> {
        let raw = raw.ok_or_else(|| anyhow::anyhow!("deploy requires a payload"))?;
        serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("deploy payload: {e}"))
    }

    /// Deploy a stack. `is_add` = true creates a fresh stack (dockge errors if it
    /// already exists); false updates an existing one.
    async fn deploy(p: StackDeployPayload, is_add: bool) -> Result<VerbOutcome> {
        let client = make_client(&p.endpoint)?;
        client
            .deploy_stack(&p.name, &p.compose_yaml, &p.compose_env, is_add)
            .await?;
        Ok(VerbOutcome::Item(ItemOutcome::new(
            Self::unit_id(&p.endpoint, &p.name),
            serde_json::to_string(&json!({
                "endpoint": p.endpoint,
                "stack": p.name,
                "deployed": true,
            }))
            .unwrap_or_default(),
        )))
    }

    async fn do_create(&self, args: CreateArgs) -> Result<VerbOutcome> {
        if args.action != "deploy" {
            return Err(anyhow::anyhow!(
                "unknown stack create action: {}",
                args.action
            ));
        }
        let p = Self::parse_deploy_payload(args.payload)?;
        // Create is add-only: dockge rejects deploying over an existing stack.
        Self::deploy(p, true).await
    }

    /// Idempotent create-or-update: add the stack if absent on the endpoint,
    /// otherwise redeploy over the existing one.
    async fn do_upsert(&self, args: UpsertArgs) -> Result<VerbOutcome> {
        let p = Self::parse_deploy_payload(args.payload)?;
        let client = make_client(&p.endpoint)?;
        let exists = client
            .list_stacks()
            .await
            .ok()
            .and_then(|v| v.as_object().map(|o| o.contains_key(&p.name)))
            .unwrap_or(false);
        Self::deploy(p, !exists).await
    }
}

impl UnitProvider for DockgeUnitProvider {
    fn name(&self) -> &str {
        "dockge"
    }

    fn declarations(&self) -> Vec<KindDeclaration> {
        vec![KindDeclaration {
            kind: KIND.into(),
            verbs: vec![
                VerbDecl::list(),
                VerbDecl::detail(),
                VerbDecl {
                    verb: Verb::Update,
                    query_schema: None,
                    actions: ACTIONS
                        .iter()
                        .map(|a| ActionDecl {
                            action: (*a).into(),
                            payload_schema: None,
                            response_schema: None,
                        })
                        .collect(),
                },
                VerbDecl {
                    verb: Verb::Delete,
                    query_schema: None,
                    actions: vec![],
                },
                VerbDecl {
                    verb: Verb::Create,
                    query_schema: None,
                    actions: vec![ActionDecl {
                        action: "deploy".into(),
                        payload_schema: Some(schema_for!(StackDeployPayload)),
                        response_schema: None,
                    }],
                },
                VerbDecl {
                    verb: Verb::Upsert,
                    query_schema: None,
                    actions: vec![ActionDecl {
                        action: "set".into(),
                        payload_schema: Some(schema_for!(StackDeployPayload)),
                        response_schema: None,
                    }],
                },
            ],
        }]
    }

    fn units(&self) -> BoxFuture<'_, Result<Vec<UnitDescriptor>>> {
        Box::pin(async move {
            let stacks = Self::all_stacks().await?;
            Ok(stacks
                .into_iter()
                .map(|(ep, name, _)| UnitDescriptor {
                    id: Self::unit_id(&ep, &name),
                    verbs: vec![Verb::List, Verb::Detail, Verb::Update, Verb::Delete],
                    parent: None,
                })
                .collect())
        })
    }

    fn invoke(&self, args: VerbArgs) -> BoxFuture<'_, Result<VerbOutcome>> {
        Box::pin(async move {
            match args {
                VerbArgs::List(a) => self.do_list(a).await,
                VerbArgs::Detail(a) => self.do_detail(a).await,
                VerbArgs::Update(a) => self.do_update(a).await,
                VerbArgs::Delete(a) => self.do_delete(a).await,
                VerbArgs::Create(a) => self.do_create(a).await,
                VerbArgs::Upsert(a) => self.do_upsert(a).await,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_round_trips_endpoint() {
        let id = DockgeUnitProvider::unit_id("baldur", "sonarr");
        assert_eq!(id.manager, "dockge@baldur");
        assert_eq!(DockgeUnitProvider::endpoint_of(&id.manager), "baldur");
        assert_eq!(id.id, "sonarr");
        assert_eq!(id.kind, "stack");
    }

    #[test]
    fn endpoint_of_tolerates_bare_manager() {
        assert_eq!(DockgeUnitProvider::endpoint_of("freyr"), "freyr");
    }

    #[test]
    fn declarations_cover_lifecycle_actions() {
        let decls = DockgeUnitProvider::new().declarations();
        let stack = decls.iter().find(|d| d.kind == "stack").unwrap();
        let update = stack.verbs.iter().find(|v| v.verb == Verb::Update).unwrap();
        for want in ACTIONS {
            assert!(
                update.actions.iter().any(|a| a.action == *want),
                "missing action {want}"
            );
        }
    }
}
