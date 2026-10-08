//! Dockge [`UnitProvider`] — compose **stacks** across every registered dockge
//! instance, surfaced as units on the generic five-verb + `action` surface.
//!
//! One dockge plugin serves many instances, so a stack's [`UnitId::manager`] is
//! `dockge@<endpoint>` — `invoke` parses the endpoint out of it and drives the
//! right instance over Socket.IO. Each stack is a `stack` kind unit. Verbs map:
//! - [`Verb::List`]   → every stack on every enabled endpoint
//! - [`Verb::Detail`] → one stack's compose YAML / env / status, plus what it
//!   leaves without orca's ownership labels
//! - [`Verb::Update`] → action `start` / `stop` / `restart` / `down` / `update`
//! - [`Verb::Delete`] → remove the stack
//! - [`Verb::Create`] → action `deploy`: `deployStack` a new stack (add-only)
//! - [`Verb::Upsert`] → action `set`: deploy add-if-absent, else update
//!
//! Deploys merge orca's ownership labels into the compose (see
//! [`crate::ownership`]) and are dry runs unless the payload sets `execute`.
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

use crate::Client;
use crate::ownership::{EnvFiles, Labeled, Previous};
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
    /// Deploy. Omitted, returns the compose that would be sent and its diff
    /// against `compose_yaml`, and changes nothing.
    #[serde(default)]
    pub execute: bool,
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
        let mut stack = client.get_stack(&args.id.id).await?;
        let global = client.global_env().await.map_err(|e| format!("{e:#}"));
        let env = EnvFiles {
            global: global.as_deref().map_err(String::as_str),
            stack: stack
                .pointer("/stack/composeENV")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        };
        let ownership = match stack.pointer("/stack/composeYAML").and_then(Value::as_str) {
            Some(yaml) => match crate::ownership::audit(yaml, &args.id.id, env) {
                Ok(c) => json!({ "complete": c.is_complete(), "unlabeled": c }),
                Err(e) => json!({ "error": format!("{e:#}") }),
            },
            None => json!({ "error": "dockge returned no compose for this stack" }),
        };
        if let Some(obj) = stack.as_object_mut() {
            obj.insert("ownership".into(), ownership);
        }
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

    /// The compose actually sent to dockge for `p`: bind propagation forced
    /// and ownership labels merged in.
    fn prepare(
        p: &StackDeployPayload,
        previous: Previous<'_>,
        global: Result<&str, &str>,
    ) -> Result<Labeled> {
        // Force `rslave` propagation on bind mounts so a host CIFS/NFS remount
        // propagates INTO the container (orca #402 Part B). Never block a
        // deploy on this transform — fall back to the original YAML.
        let (compose_yaml, mut notes) =
            crate::compose_mounts::ensure_bind_propagation(&p.compose_yaml)
                .unwrap_or_else(|_| (p.compose_yaml.clone(), Vec::new()));
        let env = EnvFiles {
            global,
            stack: &p.compose_env,
        };
        let mut labeled = crate::ownership::label(&compose_yaml, &p.name, previous, env)?;
        notes.append(&mut labeled.notes);
        labeled.notes = notes;
        Ok(labeled)
    }

    /// `labeled.notes`, plus the engine state the labels on volumes and
    /// networks new to the stack assume: the plugin cannot see whether they
    /// exist.
    fn dry_run_notes(labeled: &Labeled) -> Vec<String> {
        let mut notes = labeled.notes.clone();
        notes.extend(labeled.assumed_new.iter().map(|r| {
            format!("{r} will be labeled, assuming it does not already exist in the engine")
        }));
        notes
    }

    /// Deploy a stack. `is_add` = true creates a fresh stack (dockge errors if it
    /// already exists); false updates an existing one. A dry run unless
    /// `p.execute`.
    async fn deploy(
        client: &Client,
        p: StackDeployPayload,
        is_add: bool,
        previous: Previous<'_>,
    ) -> Result<VerbOutcome> {
        let global = client.global_env().await.map_err(|e| format!("{e:#}"));
        let labeled = Self::prepare(&p, previous, global.as_deref().map_err(String::as_str))?;
        let body = if p.execute {
            let ack = client
                .deploy_stack(&p.name, &labeled.yaml, &p.compose_env, is_add)
                .await?;
            if !crate::ack_ok(&ack) {
                return Err(anyhow::anyhow!(
                    "dockge rejected deploy of {} on {}: {}",
                    p.name,
                    p.endpoint,
                    crate::ack_msg(&ack)
                ));
            }
            json!({
                "endpoint": p.endpoint,
                "stack": p.name,
                "dryRun": false,
                "deployed": true,
                "notes": labeled.notes,
                "unlabeled": labeled.unlabeled,
            })
        } else {
            json!({
                "endpoint": p.endpoint,
                "stack": p.name,
                "dryRun": true,
                "deployed": false,
                "add": is_add,
                "diff": crate::ownership::diff(&p.compose_yaml, &labeled.yaml),
                "composeYaml": labeled.yaml,
                "notes": Self::dry_run_notes(&labeled),
                "unlabeled": labeled.unlabeled,
            })
        };
        Ok(VerbOutcome::Item(ItemOutcome::new(
            Self::unit_id(&p.endpoint, &p.name),
            serde_json::to_string(&body).unwrap_or_default(),
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
        let client = make_client(&p.endpoint)?;
        // Create is add-only: dockge rejects deploying over an existing stack.
        Self::deploy(&client, p, true, Previous::New).await
    }

    /// Idempotent create-or-update: add the stack if absent on the endpoint,
    /// otherwise redeploy over the existing one.
    async fn do_upsert(&self, args: UpsertArgs) -> Result<VerbOutcome> {
        let p = Self::parse_deploy_payload(args.payload)?;
        let client = make_client(&p.endpoint)?;
        let exists = client
            .list_stacks()
            .await?
            .as_object()
            .is_some_and(|o| o.contains_key(&p.name));
        if !exists {
            return Self::deploy(&client, p, true, Previous::New).await;
        }
        let current = client.get_stack(&p.name).await.ok();
        let field = |k: &str| {
            current
                .as_ref()
                .and_then(|s| s.pointer(&format!("/stack/{k}")))
                .and_then(Value::as_str)
        };
        let previous = match field("composeYAML") {
            Some(yaml) => Previous::Compose {
                yaml,
                env: field("composeENV").unwrap_or_default(),
            },
            None => Previous::Unknown,
        };
        Self::deploy(&client, p, false, previous).await
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
            // Dockge manages compose stacks it deploys itself; it declares no
            // restore-sufficient state to core's backup layer.
            backup_spec: None,
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

    fn payload(yaml: &str) -> StackDeployPayload {
        DockgeUnitProvider::parse_deploy_payload(Some(
            json!({ "endpoint": "baldur", "name": "media", "compose_yaml": yaml }).to_string(),
        ))
        .unwrap()
    }

    #[test]
    fn deploy_payload_defaults_to_dry_run() {
        assert!(!payload("services: {}\n").execute);
    }

    #[test]
    fn prepare_labels_and_forces_bind_propagation() {
        let p =
            payload("services:\n  app:\n    image: x\n    volumes:\n      - /srv/media:/data\n");
        let l = DockgeUnitProvider::prepare(&p, Previous::New, Ok("")).unwrap();
        let v: serde_yaml::Value = serde_yaml::from_str(&l.yaml).unwrap();
        let app = &v["services"]["app"];
        assert_eq!(app["volumes"][0]["bind"]["propagation"], "rslave");
        assert_eq!(app["labels"][crate::labels::OWNER], "dockge");
        assert_eq!(app["labels"][crate::labels::UNIT], "media");
        let d = crate::ownership::diff(&p.compose_yaml, &l.yaml);
        assert!(d.contains("+    labels:"), "{d}");
    }

    #[test]
    fn new_stack_dry_run_names_what_it_assumes_is_absent() {
        let p = payload(
            "services:\n  app:\n    image: x\n    volumes:\n      - data:/data\nvolumes:\n  data:\n",
        );
        let l = DockgeUnitProvider::prepare(&p, Previous::New, Ok("")).unwrap();
        let notes = DockgeUnitProvider::dry_run_notes(&l);
        for r in ["volume 'data'", "network 'default'"] {
            assert!(
                notes.contains(&format!(
                    "{r} will be labeled, assuming it does not already exist in the engine"
                )),
                "{notes:?}"
            );
        }
        let l = DockgeUnitProvider::prepare(
            &p,
            Previous::Compose {
                yaml: &p.compose_yaml,
                env: "",
            },
            Ok(""),
        )
        .unwrap();
        assert!(l.assumed_new.is_empty(), "{:?}", l.assumed_new);
    }

    #[test]
    fn prepare_takes_the_project_from_compose_env() {
        let mut p = payload("name: other\nservices:\n  app:\n    image: x\n");
        p.compose_env = "TZ=UTC\nCOMPOSE_PROJECT_NAME=tv\n".into();
        let l = DockgeUnitProvider::prepare(&p, Previous::New, Ok("")).unwrap();
        let v: serde_yaml::Value = serde_yaml::from_str(&l.yaml).unwrap();
        assert_eq!(v["services"]["app"]["labels"][crate::labels::STACK], "tv");
    }

    #[test]
    fn stack_env_beats_global_env_and_an_unread_global_env_labels_nothing() {
        let mut p = payload("services:\n  app:\n    image: x\n");
        let stack_of = |l: &Labeled| -> serde_yaml::Value {
            let v: serde_yaml::Value = serde_yaml::from_str(&l.yaml).unwrap();
            v["services"]["app"]["labels"][crate::labels::STACK].clone()
        };
        let global = Ok("COMPOSE_PROJECT_NAME=films\n");
        let l = DockgeUnitProvider::prepare(&p, Previous::New, global).unwrap();
        assert_eq!(stack_of(&l), "films");
        p.compose_env = "COMPOSE_PROJECT_NAME=tv\n".into();
        let l = DockgeUnitProvider::prepare(&p, Previous::New, global).unwrap();
        assert_eq!(stack_of(&l), "tv");

        p.compose_env.clear();
        let l = DockgeUnitProvider::prepare(&p, Previous::New, Err("timed out")).unwrap();
        assert_eq!(l.yaml, p.compose_yaml);
        assert_eq!(l.unlabeled, ["service 'app'", "network 'default'"]);
        assert!(
            l.notes
                .iter()
                .any(|n| n.contains("global.env could not be read") && n.contains("timed out")),
            "{:?}",
            l.notes
        );
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
