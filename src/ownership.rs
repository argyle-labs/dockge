//! Ownership labels for stacks deployed through dockge, and the per-stack
//! report of what is left unlabeled (see [`crate::labels`]).
//!
//! Dockge stores one compose file per stack and its Socket.IO API takes no
//! override file, so the labels are written into the compose YAML itself, as
//! line edits that keep the rest of the text (see [`crate::yaml_text`]). The
//! deploy dry run returns the diff; nothing is sent to dockge without
//! `execute`.
//!
//! Containers are always labeled. Labels on an existing volume or network
//! cannot change, and compose asks to recreate one whose declared labels
//! differ: a volume loses its data, and a network shared with another
//! project leaves the stack down. The plugin cannot see the engine, so the
//! stack's previously deployed compose stands in for it: a volume or network
//! it already declared keeps the orca labels it had (none if it had none),
//! and only new ones are labeled. When that compose cannot be read, every
//! declared volume and network is treated as existing and unlabeled. One with
//! an explicit `name:` may be shared outside the stack and is never labeled.
//!
//! Anonymous volumes cannot carry labels. On a new stack each is converted to
//! a named volume `<project>_<service>_<path-slug>`, as the docker plugin
//! names them; on an existing stack converting would mount a fresh, empty
//! volume, and on a replicated service one volume would replace one per
//! replica, so those are reported instead.

use std::collections::{BTreeMap, BTreeSet};

use plugin_toolkit::anyhow::{Context, Result, bail};
use plugin_toolkit::serde::Serialize;
use serde_yaml::{Mapping, Value};

use crate::compose_mounts::is_host_path;
use crate::labels::{self, Labels, OWNER_DOCKGE};
use crate::yaml_text::{Refusal, Text, quote};

/// `/var/lib/app data` → `var_lib_app_data`.
pub fn slug(path: &str) -> String {
    let s: String = path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let s = s
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if s.is_empty() { "root".to_string() } else { s }
}

/// What the stack looked like before this deploy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Previous<'a> {
    /// The stack does not exist yet.
    New,
    /// The stack exists but its compose could not be read.
    Unknown,
    /// The stack's currently deployed compose.
    Compose(&'a str),
}

/// A compose file with ownership labels written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Labeled {
    pub yaml: String,
    /// Resources left unlabeled, and why.
    pub notes: Vec<String>,
}

/// Resources a stack's compose leaves without orca's labels.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(crate = "plugin_toolkit::serde", rename_all = "camelCase")]
pub struct Coverage {
    /// The stack uses `include:` or `extends`, which this report does not
    /// follow.
    pub partial: bool,
    pub services: Vec<String>,
    pub networks: Vec<String>,
    pub volumes: Vec<String>,
    /// `<service>:<container path>`.
    pub anonymous_volumes: Vec<String>,
}

impl Coverage {
    pub fn is_complete(&self) -> bool {
        !self.partial
            && self.services.is_empty()
            && self.networks.is_empty()
            && self.volumes.is_empty()
            && self.anonymous_volumes.is_empty()
    }
}

enum Mount {
    Named { volume: String, target: String },
    Anonymous { target: String },
    Other,
}

fn mount_of(entry: &Value) -> Mount {
    match entry {
        Value::String(s) => {
            let parts: Vec<&str> = s.split(':').collect();
            match parts.as_slice() {
                [target] if target.starts_with('/') => Mount::Anonymous {
                    target: target.to_string(),
                },
                [src, target, ..]
                    if !src.is_empty() && !is_host_path(src) && !src.starts_with('$') =>
                {
                    Mount::Named {
                        volume: src.to_string(),
                        target: target.to_string(),
                    }
                }
                _ => Mount::Other,
            }
        }
        Value::Mapping(m) if m.get("type").and_then(Value::as_str) == Some("volume") => {
            let target = m
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match m.get("source").and_then(Value::as_str) {
                Some(src) if !src.is_empty() => Mount::Named {
                    volume: src.to_string(),
                    target,
                },
                _ if target.is_empty() => Mount::Other,
                _ => Mount::Anonymous { target },
            }
        }
        _ => Mount::Other,
    }
}

fn is_external(entry: Option<&Value>) -> bool {
    entry
        .and_then(|e| e.get("external"))
        .is_some_and(|x| !matches!(x, Value::Bool(false) | Value::Null))
}

/// An explicitly named volume or network already carrying orca's labels is
/// one a previous deploy converted; it is left as written.
fn is_managed(entry: Option<&Value>) -> bool {
    labels::is_managed(label_map(entry.and_then(|e| e.get("labels"))).iter())
}

fn uses_default_network(spec: &Value) -> bool {
    if spec.get("network_mode").is_some() {
        return false;
    }
    match spec.get("networks") {
        None | Some(Value::Null) => true,
        Some(Value::Sequence(s)) => s.iter().any(|n| n.as_str() == Some("default")),
        Some(Value::Mapping(m)) => m.contains_key("default"),
        Some(_) => false,
    }
}

/// `scale` / `deploy.replicas`; `None` when set to something not a number.
fn replicas(spec: &Value) -> Option<i64> {
    let scale = spec.get("scale");
    let deploy = spec.get("deploy").and_then(|d| d.get("replicas"));
    match (scale, deploy) {
        (None, None) => Some(1),
        (Some(n), None) | (None, Some(n)) => n.as_i64(),
        (Some(a), Some(b)) => a.as_i64().filter(|a| Some(*a) == b.as_i64()),
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn list_key(e: &Value) -> Option<&str> {
    let s = e.as_str()?;
    Some(s.split_once('=').map_or(s, |(k, _)| k))
}

/// A compose `labels` value, map or `k=v` list form.
fn label_map(v: Option<&Value>) -> BTreeMap<String, String> {
    match v {
        Some(Value::Mapping(m)) => m
            .iter()
            .filter_map(|(k, v)| Some((k.as_str()?.to_string(), scalar(v))))
            .collect(),
        Some(Value::Sequence(s)) => s
            .iter()
            .filter_map(Value::as_str)
            .map(|e| match e.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (e.to_string(), String::new()),
            })
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// `labels` a `<<` merge key would give `map`. Earlier merge sources win.
fn inherited_labels(map: &Mapping) -> Option<Value> {
    match map.get("<<")? {
        Value::Mapping(m) => m.get("labels").cloned(),
        Value::Sequence(s) => s.iter().find_map(|m| m.get("labels").cloned()),
        _ => None,
    }
}

/// `doc` with `<<` merge keys resolved.
fn merged(doc: &Value) -> Result<Value> {
    let mut d = doc.clone();
    d.apply_merge().context("compose merge keys")?;
    Ok(d)
}

fn has_services(doc: &Value) -> bool {
    doc.get("services").is_some_and(Value::is_mapping)
}

/// What [`label`] and [`audit`] cannot see: `include:` files and the
/// services an `extends` pulls in.
fn partial_notes(doc: &Value) -> Vec<String> {
    let mut notes = Vec::new();
    if doc.get("include").is_some() {
        notes.push("the stack uses include:; resources defined in included files are not labeled or audited".to_string());
    }
    for (svc, spec) in doc
        .get("services")
        .and_then(Value::as_mapping)
        .into_iter()
        .flatten()
    {
        if spec.get("extends").is_some() {
            notes.push(format!(
                "service '{}' uses extends; what it inherits is not labeled or audited",
                svc.as_str().unwrap_or_default()
            ));
        }
    }
    notes
}

/// `node`'s labels after [`Text::set_labels`] with the same arguments.
fn set_labels(
    node: &mut Value,
    wanted: &BTreeMap<String, String>,
    seed: &BTreeMap<String, String>,
) {
    if !node.is_mapping() {
        *node = Value::Mapping(Mapping::new());
    }
    let Some(map) = node.as_mapping_mut() else {
        return;
    };
    let to_map = |m: &BTreeMap<String, String>| -> Value {
        Value::Mapping(
            m.iter()
                .map(|(k, v)| (Value::from(k.as_str()), Value::from(v.as_str())))
                .collect(),
        )
    };
    let Some(slot) = map.get_mut("labels") else {
        let mut all = seed.clone();
        all.extend(wanted.clone());
        map.insert(Value::from("labels"), to_map(&all));
        return;
    };
    if slot.is_null() {
        *slot = Value::Mapping(Mapping::new());
    }
    match slot {
        Value::Sequence(list) => {
            for (k, v) in wanted {
                let item = Value::from(format!("{k}={v}"));
                match list.iter().position(|e| list_key(e) == Some(k.as_str())) {
                    Some(p) => {
                        if label_map(Some(&Value::Sequence(vec![list[p].clone()]))).get(k)
                            != Some(v)
                        {
                            list[p] = item;
                        }
                    }
                    None => list.push(item),
                }
            }
        }
        Value::Mapping(m) => {
            for (k, v) in wanted {
                if m.get(k.as_str()).map(scalar).as_ref() != Some(v) {
                    m.insert(Value::from(k.as_str()), Value::from(v.as_str()));
                }
            }
        }
        _ => {}
    }
}

enum Prior {
    Absent,
    Unlabeled,
    Managed(BTreeMap<String, String>),
}

/// The previous deploy, as far as it tells which volumes and networks exist.
enum Before {
    New,
    Unknown,
    Compose(Value),
}

impl Before {
    /// Dockge answers an unreadable compose with an empty string, and a file
    /// of comments parses to null: neither says what exists, so anything but
    /// a document with `services` is unknown.
    fn new(previous: Previous<'_>) -> Self {
        match previous {
            Previous::New => Before::New,
            Previous::Unknown => Before::Unknown,
            Previous::Compose(y) => match serde_yaml::from_str::<Value>(y)
                .ok()
                .filter(has_services)
                .and_then(|d| merged(&d).ok())
            {
                Some(d) => Before::Compose(d),
                None => Before::Unknown,
            },
        }
    }

    fn prior(&self, section: &str, key: &str) -> Prior {
        let doc = match self {
            Before::New => return Prior::Absent,
            Before::Unknown => return Prior::Unlabeled,
            Before::Compose(doc) => doc,
        };
        let entry = doc.get(section).and_then(|s| s.get(key));
        let existed = entry.is_some()
            || (section == "networks"
                && key == "default"
                && doc
                    .get("services")
                    .and_then(Value::as_mapping)
                    .is_some_and(|s| s.values().any(uses_default_network)));
        if !existed {
            return Prior::Absent;
        }
        let have = label_map(entry.and_then(|e| e.get("labels")));
        if labels::is_managed(have.iter()) {
            Prior::Managed(
                have.into_iter()
                    .filter(|(k, _)| k.starts_with("orca."))
                    .collect(),
            )
        } else {
            Prior::Unlabeled
        }
    }
}

fn parse(yaml: &str) -> Result<Value> {
    serde_yaml::from_str(yaml).context("compose YAML does not parse")
}

/// The text being edited and the document it must read back as.
struct Editor {
    text: Text,
    doc: Value,
    notes: Vec<String>,
}

impl Editor {
    /// Run `f`; on refusal undo whatever it changed and note why.
    fn try_edit(&mut self, what: &str, f: impl FnOnce(&mut Self) -> Result<(), Refusal>) -> bool {
        let saved = (self.text.clone(), self.doc.clone());
        match f(self) {
            Ok(()) => true,
            Err(why) => {
                (self.text, self.doc) = saved;
                self.notes.push(format!("{what}: {why}"));
                false
            }
        }
    }

    fn node(&mut self, section: &str, key: &str) -> Option<&mut Value> {
        self.doc.get_mut(section).and_then(|s| s.get_mut(key))
    }

    /// Set `wanted` on the existing node `section.key`.
    fn label(&mut self, section: &str, key: &str, wanted: &BTreeMap<String, String>, what: &str) {
        let raw = self
            .doc
            .get(section)
            .and_then(|s| s.get(key))
            .cloned()
            .unwrap_or(Value::Null);
        let own = raw.get("labels").map(|l| label_map(Some(l)));
        let seed = match (&raw, &own) {
            (Value::Mapping(m), None) => label_map(inherited_labels(m).as_ref()),
            _ => BTreeMap::new(),
        };
        let effective = own.clone().unwrap_or_else(|| seed.clone());
        if wanted.iter().all(|(k, v)| effective.get(k) == Some(v)) {
            return;
        }
        self.try_edit(&format!("{what} not labeled"), |e| {
            let at = e.text.path(&[section, key])?;
            e.text.set_labels(at, wanted, own.as_ref(), &seed)?;
            if let Some(node) = e.node(section, key) {
                set_labels(node, wanted, &seed);
            }
            Ok(())
        });
    }

    /// Add the entry `section.key` with `body`, creating the section.
    fn add(&mut self, section: &str, key: &str, body: Mapping) -> Result<(), Refusal> {
        self.text.add_entry(section, key, &body)?;
        let root = self
            .doc
            .as_mapping_mut()
            .ok_or("the compose root is not a mapping")?;
        let sec = root
            .entry(Value::from(section))
            .or_insert(Value::Mapping(Mapping::new()));
        if sec.is_null() {
            *sec = Value::Mapping(Mapping::new());
        }
        sec.as_mapping_mut()
            .ok_or_else(|| format!("top-level '{section}' is not a mapping"))?
            .insert(Value::from(key), Value::Mapping(body));
        Ok(())
    }

    /// Point anonymous mount `i` of `svc` at named volume `volume`.
    fn convert(&mut self, svc: &str, i: usize, volume: &str, target: &str) -> Result<(), Refusal> {
        let at = self.text.path(&["services", svc])?;
        let item = self.text.seq_item(at, "volumes", i)?;
        let slot = self
            .doc
            .get_mut("services")
            .and_then(|s| s.get_mut(svc))
            .and_then(|s| s.get_mut("volumes"))
            .and_then(|v| v.get_mut(i))
            .ok_or("its volumes are inherited through <<")?;
        match slot {
            Value::Mapping(m) => {
                if m.contains_key("source") {
                    return Err("it sets an empty source".into());
                }
                self.text
                    .item_append(item, &[format!("source: {}", quote(volume))])?;
                m.insert(Value::from("source"), Value::from(volume));
            }
            other => {
                let new = format!("{volume}:{target}");
                let ind = self.text.item_indent(item);
                self.text
                    .replace_item(item, vec![format!("{}- {}", " ".repeat(ind), quote(&new))])?;
                *other = Value::from(new);
            }
        }
        Ok(())
    }
}

fn labels_body(labels: &BTreeMap<String, String>) -> Mapping {
    let mut m = Mapping::new();
    m.insert(
        Value::from("labels"),
        Value::Mapping(
            labels
                .iter()
                .map(|(k, v)| (Value::from(k.as_str()), Value::from(v.as_str())))
                .collect(),
        ),
    );
    m
}

/// Write orca's ownership labels into `compose_yaml` for dockge stack
/// `stack`. Unchanged input is returned byte-for-byte; so is the input when
/// the edited text would not read back as the intended document.
pub fn label(compose_yaml: &str, stack: &str, previous: Previous<'_>) -> Result<Labeled> {
    let original = parse(compose_yaml)?;
    if !original.is_mapping() {
        bail!("compose is empty or not a YAML mapping");
    }
    let resolved = merged(&original)?;
    let before = Before::new(previous);
    let mut notes = partial_notes(&resolved);
    let unchanged = |notes| {
        Ok(Labeled {
            yaml: compose_yaml.to_string(),
            notes,
        })
    };
    let Some(services) = resolved.get("services").and_then(Value::as_mapping) else {
        return unchanged(notes);
    };
    let text = match Text::new(compose_yaml) {
        Ok(t) => t,
        Err(why) => {
            notes.push(format!("labels not applied: {why}"));
            return unchanged(notes);
        }
    };
    let project = resolved
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(stack)
        .to_string();
    let mut e = Editor {
        text,
        doc: original.clone(),
        notes,
    };

    let mut mounts: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut anonymous: Vec<(String, usize, String, Option<i64>)> = Vec::new();
    let mut default_used = false;
    for (key, spec) in services {
        let Some(svc) = key.as_str() else {
            continue;
        };
        default_used |= uses_default_network(spec);
        let entries = spec.get("volumes").and_then(Value::as_sequence);
        for (i, entry) in entries.into_iter().flatten().enumerate() {
            match mount_of(entry) {
                Mount::Named { volume, target } => mounts
                    .entry(volume)
                    .or_default()
                    .push((svc.to_string(), target)),
                Mount::Anonymous { target } => {
                    anonymous.push((svc.to_string(), i, target, replicas(spec)))
                }
                Mount::Other => {}
            }
        }
        let wanted = Labels::for_(OWNER_DOCKGE, &project, Some(svc), Some(stack)).to_map();
        e.label("services", svc, &wanted, &format!("service '{svc}'"));
    }

    let declared: BTreeMap<String, Value> = resolved
        .get("volumes")
        .and_then(Value::as_mapping)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.clone())))
                .collect()
        })
        .unwrap_or_default();
    let mut taken: BTreeSet<String> = declared.keys().cloned().collect();
    let mut engine_names: BTreeSet<String> = declared
        .iter()
        .map(|(k, v)| match v.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None => format!("{project}_{k}"),
        })
        .collect();
    for (svc, i, target, replicas) in anonymous {
        let what = format!("service '{svc}' anonymous volume at {target}");
        if !matches!(before, Before::New) {
            e.notes.push(format!(
                "{what} cannot carry labels; not converted on an existing stack (it would mount a new, empty volume)"
            ));
            continue;
        }
        if replicas != Some(1) {
            e.notes.push(format!(
                "{what} cannot carry labels; not converted on a service with more than one replica (they would share one volume)"
            ));
            continue;
        }
        let volume = format!("{svc}_{}", slug(&target));
        let name = format!("{project}_{volume}");
        if taken.contains(&volume) || engine_names.contains(&name) {
            e.notes.push(format!(
                "{what} not converted: volume '{volume}' ({name}) is already declared"
            ));
            continue;
        }
        let mut body = Mapping::new();
        body.insert(Value::from("name"), Value::from(name.as_str()));
        body.extend(labels_body(
            &Labels::for_(OWNER_DOCKGE, &project, Some(&svc), Some(stack))
                .with_mount(&target)
                .to_map(),
        ));
        if e.try_edit(&format!("{what} not converted"), |e| {
            e.add("volumes", &volume, body)?;
            e.convert(&svc, i, &volume, &target)
        }) {
            taken.insert(volume);
            engine_names.insert(name);
        }
    }

    for (key, entry) in &declared {
        if is_external(Some(entry)) {
            continue;
        }
        if entry.get("name").is_some() {
            if is_managed(Some(entry)) {
                continue;
            }
            e.notes.push(format!(
                "volume '{key}' sets an explicit name; not labeled (it may be shared outside this stack)"
            ));
            continue;
        }
        let wanted = match mounts.get(key).map(Vec::as_slice) {
            Some([(svc, target)]) => {
                Labels::for_(OWNER_DOCKGE, &project, Some(svc), Some(stack)).with_mount(target)
            }
            _ => Labels::for_(OWNER_DOCKGE, &project, None, Some(stack)),
        };
        match before.prior("volumes", key) {
            Prior::Absent => e.label("volumes", key, &wanted.to_map(), &format!("volume '{key}'")),
            Prior::Managed(have) => e.label("volumes", key, &have, &format!("volume '{key}'")),
            Prior::Unlabeled => e.notes.push(format!(
                "volume '{key}' was deployed without orca labels; not relabeled (compose would recreate it)"
            )),
        }
    }

    let declared_networks = resolved.get("networks").and_then(Value::as_mapping);
    let mut networks: BTreeSet<String> = declared_networks
        .map(|m| {
            m.keys()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if default_used {
        networks.insert("default".to_string());
    }
    for key in networks {
        let entry = declared_networks.and_then(|n| n.get(key.as_str()));
        if is_external(entry) {
            continue;
        }
        if entry.and_then(|n| n.get("name")).is_some() {
            if is_managed(entry) {
                continue;
            }
            e.notes.push(format!(
                "network '{key}' sets an explicit name; not labeled (it may be shared outside this stack)"
            ));
            continue;
        }
        let what = format!("network '{key}'");
        let labels = match before.prior("networks", &key) {
            Prior::Absent => Labels::for_(OWNER_DOCKGE, &project, None, Some(stack)).to_map(),
            Prior::Managed(have) => have,
            Prior::Unlabeled => {
                e.notes.push(format!(
                    "{what} was deployed without orca labels; not relabeled (compose would recreate it)"
                ));
                continue;
            }
        };
        if entry.is_some() {
            e.label("networks", &key, &labels, &what);
        } else {
            e.try_edit(&format!("{what} not labeled"), |e| {
                e.add("networks", &key, labels_body(&labels))
            });
        }
    }

    let Editor {
        text,
        doc,
        mut notes,
    } = e;
    if doc == original {
        return unchanged(notes);
    }
    let yaml = text.render();
    if serde_yaml::from_str::<Value>(&yaml).ok().as_ref() != Some(&doc) {
        notes.push(
            "labels not applied: the edited compose did not read back as intended".to_string(),
        );
        return unchanged(notes);
    }
    Ok(Labeled { yaml, notes })
}

/// What `compose_yaml` leaves without orca's labels.
pub fn audit(compose_yaml: &str) -> Result<Coverage> {
    let doc = merged(&parse(compose_yaml)?)?;
    if !doc.is_mapping() {
        bail!("compose is empty or not a YAML mapping");
    }
    let mut out = Coverage {
        partial: !partial_notes(&doc).is_empty(),
        ..Coverage::default()
    };
    let Some(services) = doc.get("services").and_then(Value::as_mapping) else {
        if out.partial {
            return Ok(out);
        }
        bail!("compose has no services");
    };
    let mut default_used = false;
    for (key, spec) in services {
        let Some(svc) = key.as_str() else {
            continue;
        };
        default_used |= uses_default_network(spec);
        if !labels::is_managed(label_map(spec.get("labels")).iter()) {
            out.services.push(svc.to_string());
        }
        let entries = spec.get("volumes").and_then(Value::as_sequence);
        for entry in entries.into_iter().flatten() {
            if let Mount::Anonymous { target } = mount_of(entry) {
                out.anonymous_volumes.push(format!("{svc}:{target}"));
            }
        }
    }
    let unlabeled = |section: &str, extra: Option<&str>| -> Vec<String> {
        let declared = doc.get(section).and_then(Value::as_mapping);
        let mut keys: BTreeSet<String> = declared
            .map(|m| {
                m.keys()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        keys.extend(extra.map(str::to_string));
        keys.into_iter()
            .filter(|k| {
                let entry = declared.and_then(|m| m.get(k.as_str()));
                !is_external(entry) && !is_managed(entry)
            })
            .collect()
    };
    out.volumes = unlabeled("volumes", None);
    out.networks = unlabeled("networks", default_used.then_some("default"));
    Ok(out)
}

/// Line diff of `old` → `new`: unchanged lines prefixed `' '`, removed `-`,
/// added `+`. Empty when equal.
pub fn diff(old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut out = String::new();
    // The LCS table is quadratic; past this, list the whole replacement.
    if a.len().saturating_mul(b.len()) > 4_000_000 {
        a.iter().for_each(|l| out.push_str(&format!("-{l}\n")));
        b.iter().for_each(|l| out.push_str(&format!("+{l}\n")));
        return out;
    }
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push_str(&format!(" {}\n", a[i]));
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            out.push_str(&format!("-{}\n", a[i]));
            i += 1;
        } else {
            out.push_str(&format!("+{}\n", b[j]));
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::{MANAGED, MOUNT, OWNER, SERVICE, STACK, UNIT};

    fn labeled(yaml: &str, previous: Previous<'_>) -> (Value, Vec<String>) {
        let l = label(yaml, "media", previous).unwrap();
        (serde_yaml::from_str(&l.yaml).unwrap(), l.notes)
    }

    #[test]
    fn slug_matches_the_docker_plugin() {
        assert_eq!(slug("/var/lib/App Data/"), "var_lib_app_data");
        assert_eq!(slug("/"), "root");
    }

    #[test]
    fn new_stack_labels_services_networks_and_volumes() {
        let yaml = "services:\n  app:\n    image: x\n    volumes:\n      - data:/data\n  worker:\n    image: y\n    networks: [backend]\nnetworks:\n  backend: {}\n  proxy:\n    external: true\nvolumes:\n  data:\n";
        let (v, notes) = labeled(yaml, Previous::New);
        assert!(notes.is_empty(), "{notes:?}");
        let app = &v["services"]["app"]["labels"];
        assert_eq!(app[MANAGED], "true");
        assert_eq!(app[OWNER], "dockge");
        assert_eq!(app[STACK], "media");
        assert_eq!(app[SERVICE], "app");
        assert_eq!(app[UNIT], "media");
        assert_eq!(v["services"]["worker"]["labels"][SERVICE], "worker");
        assert_eq!(v["networks"]["backend"]["labels"][MANAGED], "true");
        assert!(v["networks"]["backend"]["labels"].get(SERVICE).is_none());
        assert!(v["networks"]["proxy"].get("labels").is_none(), "external");
        assert_eq!(v["networks"]["default"]["labels"][STACK], "media");
        let data = &v["volumes"]["data"]["labels"];
        assert_eq!(data[SERVICE], "app");
        assert_eq!(data[MOUNT], "/data");
    }

    #[test]
    fn project_name_comes_from_the_compose_name_key() {
        let (v, _) = labeled("name: tv\nservices:\n  app:\n    image: x\n", Previous::New);
        assert_eq!(v["services"]["app"]["labels"][STACK], "tv");
        assert_eq!(v["services"]["app"]["labels"][UNIT], "media");
    }

    #[test]
    fn list_form_labels_keep_their_form_and_operator_entries() {
        let yaml = "services:\n  app:\n    image: x\n    network_mode: host\n    labels:\n      - orca.heal=false\n      - orca.owner=someone\n";
        let (v, _) = labeled(yaml, Previous::New);
        let list: Vec<&str> = v["services"]["app"]["labels"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(list.contains(&"orca.heal=false"));
        assert!(list.contains(&"orca.owner=dockge"));
        assert!(!list.contains(&"orca.owner=someone"));
        assert!(v.get("networks").is_none(), "host network mode");
    }

    #[test]
    fn merge_key_labels_are_kept_when_adding_own_labels() {
        let yaml = "x-base: &base\n  image: x\n  labels:\n    team: media\nservices:\n  app:\n    <<: *base\n";
        let (v, _) = labeled(yaml, Previous::New);
        let app = &v["services"]["app"]["labels"];
        assert_eq!(app["team"], "media");
        assert_eq!(app[MANAGED], "true");
    }

    #[test]
    fn new_stack_converts_anonymous_volumes_to_named() {
        let yaml = "services:\n  app:\n    image: x\n    network_mode: none\n    volumes:\n      - /cache\n      - type: volume\n        target: /var/lib/db\n";
        let (v, notes) = labeled(yaml, Previous::New);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(v["services"]["app"]["volumes"][0], "app_cache:/cache");
        assert_eq!(
            v["services"]["app"]["volumes"][1]["source"],
            "app_var_lib_db"
        );
        let cache = &v["volumes"]["app_cache"];
        assert_eq!(cache["name"], "media_app_cache");
        assert_eq!(cache["labels"][MOUNT], "/cache");
        assert_eq!(cache["labels"][SERVICE], "app");
        assert_eq!(
            v["volumes"]["app_var_lib_db"]["labels"][MOUNT],
            "/var/lib/db"
        );
        assert!(
            audit(&serde_yaml::to_string(&v).unwrap())
                .unwrap()
                .is_complete()
        );
    }

    #[test]
    fn existing_stack_reports_anonymous_volumes_instead_of_converting() {
        let yaml = "services:\n  app:\n    image: x\n    volumes:\n      - /cache\n";
        let (v, notes) = labeled(yaml, Previous::Compose(yaml));
        assert_eq!(v["services"]["app"]["volumes"][0], "/cache");
        assert!(v.get("volumes").is_none());
        assert!(notes.iter().any(|n| n.contains("/cache")), "{notes:?}");
    }

    #[test]
    fn existing_unlabeled_volumes_and_networks_are_not_relabeled() {
        let yaml = "services:\n  app:\n    image: x\n    volumes:\n      - data:/data\nvolumes:\n  data:\n";
        let (v, notes) = labeled(yaml, Previous::Compose(yaml));
        assert!(v["volumes"]["data"].get("labels").is_none());
        assert!(v.get("networks").is_none());
        assert_eq!(v["services"]["app"]["labels"][MANAGED], "true");
        assert!(notes.iter().any(|n| n.contains("volume 'data'")));
        assert!(notes.iter().any(|n| n.contains("network 'default'")));
    }

    #[test]
    fn existing_stack_labels_only_new_volumes_and_keeps_orca_labels() {
        let old = "services:\n  app:\n    image: x\n    network_mode: host\n    volumes:\n      - data:/data\nvolumes:\n  data:\n    labels:\n      orca.managed: 'true'\n      orca.owner: dockge\n      orca.stack: old\n";
        let new = "services:\n  app:\n    image: x\n    network_mode: host\n    volumes:\n      - data:/data\n      - logs:/logs\nvolumes:\n  data:\n  logs:\n";
        let (v, notes) = labeled(new, Previous::Compose(old));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(v["volumes"]["data"]["labels"][STACK], "old");
        assert!(v["volumes"]["data"]["labels"].get(SERVICE).is_none());
        assert_eq!(v["volumes"]["logs"]["labels"][STACK], "media");
    }

    #[test]
    fn unknown_previous_treats_every_volume_as_existing() {
        let yaml = "services:\n  app:\n    image: x\n    network_mode: host\n    volumes:\n      - data:/data\nvolumes:\n  data:\n";
        let (v, notes) = labeled(yaml, Previous::Unknown);
        assert!(v["volumes"]["data"].get("labels").is_none());
        assert_eq!(notes.len(), 1, "{notes:?}");
    }

    #[test]
    fn labeling_is_idempotent_and_returns_unchanged_input_verbatim() {
        let yaml = "services:\n  app:\n    image: x\n    volumes:\n      - /cache\n";
        let once = label(yaml, "media", Previous::New).unwrap().yaml;
        let twice = label(&once, "media", Previous::Compose(&once)).unwrap();
        assert_eq!(twice.yaml, once);
        assert!(twice.notes.is_empty(), "{:?}", twice.notes);
    }

    #[test]
    fn unparseable_compose_is_refused() {
        assert!(label("a: : :\n - b", "media", Previous::New).is_err());
    }

    #[test]
    fn audit_reports_unlabeled_resources() {
        let yaml = "services:\n  app:\n    image: x\n    volumes:\n      - data:/data\n      - /cache\n      - ./conf:/conf\nnetworks:\n  proxy:\n    external: true\nvolumes:\n  data:\n";
        let c = audit(yaml).unwrap();
        assert_eq!(c.services, ["app"]);
        assert_eq!(c.networks, ["default"]);
        assert_eq!(c.volumes, ["data"]);
        assert_eq!(c.anonymous_volumes, ["app:/cache"]);
        assert!(!c.is_complete());
    }

    #[test]
    fn empty_or_comment_only_previous_compose_is_unknown() {
        let yaml = "services:\n  app:\n    image: x\n    volumes:\n      - data:/data\n      - /cache\nvolumes:\n  data:\n";
        for previous in ["", "# nothing here\n"] {
            let (v, notes) = labeled(yaml, Previous::Compose(previous));
            assert!(v["volumes"]["data"].get("labels").is_none(), "{previous:?}");
            assert!(v.get("networks").is_none(), "{previous:?}");
            assert_eq!(v["services"]["app"]["volumes"][1], "/cache");
            assert_eq!(notes.len(), 3, "{notes:?}");
        }
    }

    #[test]
    fn edits_keep_comments_anchors_and_quoting() {
        let yaml = "# media stack\nx-common: &common\n  restart: unless-stopped # always\n  environment:\n    COUNT: '1_000'\n    HEX: '0x_1F'\n    F: '1_0.5'\nservices:\n  app:\n    <<: *common\n    image: x # pinned\n  worker:\n    <<: *common\n    image: y\n";
        let l = label(yaml, "media", Previous::New).unwrap();
        assert!(l.notes.is_empty(), "{:?}", l.notes);
        for kept in [
            "# media stack\n",
            "x-common: &common\n",
            "restart: unless-stopped # always\n",
            "COUNT: '1_000'\n",
            "HEX: '0x_1F'\n",
            "F: '1_0.5'\n",
            "<<: *common\n",
            "image: x # pinned\n",
        ] {
            assert!(l.yaml.contains(kept), "lost {kept:?} in\n{}", l.yaml);
        }
        let d = diff(yaml, &l.yaml);
        assert!(d.lines().all(|x| !x.starts_with('-')), "{d}");
        let mut v: Value = serde_yaml::from_str(&l.yaml).unwrap();
        v.apply_merge().unwrap();
        assert_eq!(v["services"]["worker"]["environment"]["COUNT"], "1_000");
        assert_eq!(v["services"]["worker"]["labels"][SERVICE], "worker");
    }

    #[test]
    fn flow_style_service_is_refused_with_a_note() {
        let yaml = "services:\n  app: {image: x}\n  web:\n    image: y\n";
        let (v, notes) = labeled(yaml, Previous::New);
        assert!(v["services"]["app"].get("labels").is_none());
        assert_eq!(v["services"]["web"]["labels"][SERVICE], "web");
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("service 'app' not labeled")),
            "{notes:?}"
        );
    }

    #[test]
    fn anchored_labels_are_refused_with_a_note() {
        let yaml = "services:\n  app:\n    image: x\n    labels: &l\n      team: a\n  web:\n    image: y\n    labels: *l\n";
        let (v, notes) = labeled(yaml, Previous::New);
        assert!(v["services"]["app"]["labels"].get(MANAGED).is_none());
        assert!(v["services"]["web"]["labels"].get(MANAGED).is_none());
        assert_eq!(notes.len(), 2, "{notes:?}");
    }

    #[test]
    fn explicitly_named_volumes_and_networks_are_never_labeled() {
        let yaml = "services:\n  app:\n    image: x\n    networks: [shared]\n    volumes:\n      - data:/data\nnetworks:\n  shared:\n    name: proxy\nvolumes:\n  data:\n    name: media-data\n";
        let (v, notes) = labeled(yaml, Previous::New);
        assert!(v["volumes"]["data"].get("labels").is_none());
        assert!(v["networks"]["shared"].get("labels").is_none());
        assert!(
            notes
                .iter()
                .any(|n| n.contains("volume 'data' sets an explicit name"))
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("network 'shared' sets an explicit name"))
        );
    }

    #[test]
    fn replicated_services_keep_anonymous_volumes() {
        for scale in [
            "    scale: 2\n",
            "    deploy:\n      replicas: 3\n",
            "    scale: ${N}\n",
        ] {
            let yaml = format!(
                "services:\n  app:\n    image: x\n    network_mode: host\n{scale}    volumes:\n      - /cache\n"
            );
            let (v, notes) = labeled(&yaml, Previous::New);
            assert_eq!(v["services"]["app"]["volumes"][0], "/cache", "{scale}");
            assert!(
                notes.iter().any(|n| n.contains("more than one replica")),
                "{notes:?}"
            );
        }
        let yaml = "services:\n  app:\n    image: x\n    network_mode: host\n    deploy:\n      replicas: 1\n    volumes:\n      - /cache\n";
        let (v, _) = labeled(yaml, Previous::New);
        assert_eq!(v["services"]["app"]["volumes"][0], "app_cache:/cache");
    }

    #[test]
    fn conversion_skips_a_name_another_volume_already_uses() {
        let yaml = "services:\n  app:\n    image: x\n    network_mode: host\n    volumes:\n      - /cache\n      - other:/other\nvolumes:\n  other:\n    name: media_app_cache\n";
        let (v, notes) = labeled(yaml, Previous::New);
        assert_eq!(v["services"]["app"]["volumes"][0], "/cache");
        assert!(
            notes.iter().any(|n| n.contains("already declared")),
            "{notes:?}"
        );
    }

    #[test]
    fn merge_key_volumes_count_for_mounts_and_default_network() {
        let yaml = "x-base: &base\n  image: x\n  network_mode: host\n  volumes:\n    - data:/data\nservices:\n  app:\n    <<: *base\nvolumes:\n  data:\n";
        let (v, _) = labeled(yaml, Previous::New);
        assert_eq!(v["volumes"]["data"]["labels"][SERVICE], "app");
        assert!(v.get("networks").is_none(), "network_mode inherited");
    }

    #[test]
    fn include_and_extends_are_partial() {
        let yaml = "include:\n  - other.yaml\nservices:\n  app:\n    extends:\n      file: base.yaml\n      service: base\n    labels:\n      orca.managed: 'true'\n    network_mode: host\n";
        let c = audit(yaml).unwrap();
        assert!(c.partial);
        assert!(c.services.is_empty());
        assert!(!c.is_complete());
        let (_, notes) = labeled(yaml, Previous::New);
        assert!(notes.iter().any(|n| n.contains("include:")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("extends")), "{notes:?}");
    }

    #[test]
    fn audit_refuses_an_empty_compose() {
        assert!(audit("").is_err());
        assert!(audit("# just comments\n").is_err());
        assert!(label("", "media", Previous::New).is_err());
    }

    /// The input lines of a `diff` with no removals, i.e. the output with the
    /// inserted lines taken out.
    fn kept(old: &str, new: &str) -> String {
        let d = diff(old, new);
        assert!(d.lines().all(|l| !l.starts_with('-')), "{d}");
        d.lines()
            .filter_map(|l| l.strip_prefix(' '))
            .map(|l| format!("{l}\n"))
            .collect()
    }

    const OPERATOR_COMPOSE: &str = "# media stack, hand-edited\nservices:\n  app: # main\n    image: \"ghcr.io/x/app:1.2\" # pinned\n    user: \"1000\"\n    ports:\n      - \"8080:8080\"\n      - '9090:9090' # metrics\n    environment:\n      PORT: \"8080\"\n      UMASK: '0755'\n      RATIO: '1_000'\n    labels:\n      team: media # owner\n    volumes:\n      - data:/data\n    networks:\n      - backend\n\n  # sidecar\n  worker:\n    image: y\n    network_mode: host\nnetworks:\n  backend: # internal\n    driver: bridge\nvolumes:\n  data:\n    driver: local # default\n";

    #[test]
    fn operator_text_survives_byte_for_byte_outside_inserted_labels() {
        let l = label(OPERATOR_COMPOSE, "media", Previous::New).unwrap();
        assert!(l.notes.is_empty(), "{:?}", l.notes);
        assert_ne!(l.yaml, OPERATOR_COMPOSE);
        assert_eq!(kept(OPERATOR_COMPOSE, &l.yaml), OPERATOR_COMPOSE);
        let added: Vec<String> = diff(OPERATOR_COMPOSE, &l.yaml)
            .lines()
            .filter_map(|x| x.strip_prefix('+').map(str::to_string))
            .collect();
        assert!(
            added
                .iter()
                .all(|x| x.trim_start().starts_with("orca.") || x.trim() == "labels:"),
            "{added:?}"
        );
        let v: Value = serde_yaml::from_str(&l.yaml).unwrap();
        let env = &v["services"]["app"]["environment"];
        assert_eq!(env["PORT"], "8080");
        assert_eq!(env["UMASK"], "0755");
        assert_eq!(env["RATIO"], "1_000");
        assert_eq!(v["services"]["app"]["user"], "1000");
        assert_eq!(v["services"]["app"]["ports"][0], "8080:8080");
        assert_eq!(v["services"]["app"]["labels"]["team"], "media");
        assert_eq!(v["services"]["app"]["labels"][SERVICE], "app");
        assert_eq!(v["networks"]["backend"]["labels"][MANAGED], "true");
        assert_eq!(v["volumes"]["data"]["labels"][MOUNT], "/data");
    }

    #[test]
    fn empty_or_absent_previous_never_labels_existing_networks_or_volumes() {
        let previous = [
            Previous::Unknown,
            Previous::Compose(""),
            Previous::Compose("\n"),
            Previous::Compose("# nothing\n"),
            Previous::Compose("{}\n"),
            Previous::Compose("services:\n"),
            Previous::Compose("not: [valid"),
        ];
        for p in previous {
            let l = label(OPERATOR_COMPOSE, "media", p).unwrap();
            let v: Value = serde_yaml::from_str(&l.yaml).unwrap();
            assert!(v["networks"]["backend"].get("labels").is_none(), "{p:?}");
            assert!(v["volumes"]["data"].get("labels").is_none(), "{p:?}");
            assert!(v["networks"].get("default").is_none(), "{p:?}");
            assert_eq!(v["services"]["app"]["labels"][MANAGED], "true", "{p:?}");
            for name in ["network 'backend'", "volume 'data'"] {
                assert!(
                    l.notes.iter().any(|n| n.starts_with(name)),
                    "{p:?}: {:?}",
                    l.notes
                );
            }
            let tail = OPERATOR_COMPOSE.split_once("\nnetworks:\n").unwrap().1;
            assert!(l.yaml.ends_with(tail), "{p:?}\n{}", l.yaml);
            assert_eq!(kept(OPERATOR_COMPOSE, &l.yaml), OPERATOR_COMPOSE);
        }
    }

    #[test]
    fn rerunning_on_its_own_output_changes_nothing() {
        let anonymous = "services:\n  app:\n    image: x\n    volumes:\n      - /cache\n";
        for yaml in [OPERATOR_COMPOSE, anonymous] {
            let once = label(yaml, "media", Previous::New).unwrap();
            assert!(once.notes.is_empty(), "{:?}", once.notes);
            let twice = label(&once.yaml, "media", Previous::Compose(&once.yaml)).unwrap();
            assert_eq!(twice.yaml, once.yaml);
            assert!(twice.notes.is_empty(), "{:?}", twice.notes);
            let thrice = label(&twice.yaml, "media", Previous::Compose(&once.yaml)).unwrap();
            assert_eq!(thrice.yaml, once.yaml);
            assert_eq!(diff(&once.yaml, &thrice.yaml), "");
        }
    }

    #[test]
    fn diff_marks_added_and_removed_lines() {
        assert_eq!(diff("a\nb\n", "a\nb\n"), "");
        assert_eq!(diff("a\nb\nc\n", "a\nx\nc\n"), " a\n-b\n+x\n c\n");
    }
}
