//! Ownership labels for stacks deployed through dockge, and the per-stack
//! report of what is left unlabeled (see [`crate::labels`]).
//!
//! Dockge stores one compose file per stack and its Socket.IO API takes no
//! override file, so the labels are merged into the compose YAML itself. The
//! deploy dry run returns the diff; nothing is sent to dockge without
//! `execute`.
//!
//! Containers are always labeled. Labels on an existing volume or network
//! cannot change, and compose asks to recreate one whose declared labels
//! differ: a volume loses its data, and a network shared with another
//! project leaves the stack down. The plugin cannot see the engine, so the
//! stack's previously deployed compose stands in for it: a volume or network
//! it already declared keeps the orca labels it had (none if it had none),
//! and only new ones are labeled.
//!
//! Anonymous volumes cannot carry labels. On a new stack each is converted to
//! a named volume `<project>_<service>_<path-slug>`, as the docker plugin
//! names them; on an existing stack converting would mount a fresh, empty
//! volume, so they are reported instead.

use std::collections::{BTreeMap, BTreeSet};

use plugin_toolkit::anyhow::{Context, Result};
use plugin_toolkit::serde::Serialize;
use serde_yaml::{Mapping, Value};

use crate::compose_mounts::is_host_path;
use crate::labels::{self, Labels, OWNER_DOCKGE};

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

/// A compose file with ownership labels merged in.
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
    pub services: Vec<String>,
    pub networks: Vec<String>,
    pub volumes: Vec<String>,
    /// `<service>:<container path>`.
    pub anonymous_volumes: Vec<String>,
}

impl Coverage {
    pub fn is_complete(&self) -> bool {
        self.services.is_empty()
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

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
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

fn own_or_inherited_labels(entry: Option<&Value>) -> BTreeMap<String, String> {
    match entry {
        Some(Value::Mapping(m)) => match m.get("labels") {
            Some(l) => label_map(Some(l)),
            None => label_map(inherited_labels(m).as_ref()),
        },
        _ => BTreeMap::new(),
    }
}

/// Set `wanted` in `entry`'s labels, keeping every other label and the
/// form (map or list) the operator used.
fn merge_labels(entry: &mut Value, wanted: &BTreeMap<String, String>) {
    if !entry.is_mapping() {
        *entry = Value::Mapping(Mapping::new());
    }
    let Some(map) = entry.as_mapping_mut() else {
        return;
    };
    if !map.contains_key("labels") {
        // An own `labels` key replaces, not merges, one inherited through `<<`.
        let seed = inherited_labels(map).unwrap_or(Value::Mapping(Mapping::new()));
        map.insert(Value::from("labels"), seed);
    }
    let Some(slot) = map.get_mut("labels") else {
        return;
    };
    match slot {
        Value::Sequence(list) => {
            list.retain(|e| {
                e.as_str()
                    .map(|s| s.split_once('=').map_or(s, |(k, _)| k))
                    .is_none_or(|k| !wanted.contains_key(k))
            });
            list.extend(wanted.iter().map(|(k, v)| Value::from(format!("{k}={v}"))));
        }
        Value::Mapping(m) => {
            for (k, v) in wanted {
                m.insert(Value::from(k.as_str()), Value::from(v.as_str()));
            }
        }
        other => {
            *other = Value::Mapping(
                wanted
                    .iter()
                    .map(|(k, v)| (Value::from(k.as_str()), Value::from(v.as_str())))
                    .collect(),
            );
        }
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
        let have = own_or_inherited_labels(entry);
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

/// Merge orca's ownership labels into `compose_yaml` for dockge stack
/// `stack`. Unchanged input is returned byte-for-byte.
pub fn label(compose_yaml: &str, stack: &str, previous: Previous<'_>) -> Result<Labeled> {
    let original = parse(compose_yaml)?;
    let before = match previous {
        Previous::New => Before::New,
        Previous::Unknown => Before::Unknown,
        Previous::Compose(y) => serde_yaml::from_str(y).map_or(Before::Unknown, Before::Compose),
    };
    let mut doc = original.clone();
    let mut notes = Vec::new();
    let project = doc
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(stack)
        .to_string();

    let Some(services) = doc.get_mut("services").and_then(Value::as_mapping_mut) else {
        return Ok(Labeled {
            yaml: compose_yaml.to_string(),
            notes,
        });
    };

    let mut mounts: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut anonymous: Vec<(String, usize, String)> = Vec::new();
    let mut default_used = false;
    for (key, spec) in services.iter_mut() {
        let Some(svc) = key.as_str().map(str::to_string) else {
            continue;
        };
        default_used |= uses_default_network(spec);
        let entries = spec.get("volumes").and_then(Value::as_sequence);
        for (i, entry) in entries.into_iter().flatten().enumerate() {
            match mount_of(entry) {
                Mount::Named { volume, target } => mounts
                    .entry(volume)
                    .or_default()
                    .push((svc.clone(), target)),
                Mount::Anonymous { target } => anonymous.push((svc.clone(), i, target)),
                Mount::Other => {}
            }
        }
        let wanted = Labels::for_(OWNER_DOCKGE, &project, Some(&svc), Some(stack)).to_map();
        merge_labels(spec, &wanted);
    }

    let mut conversions = Vec::new();
    let declared: BTreeSet<String> = doc
        .get("volumes")
        .and_then(Value::as_mapping)
        .map(|m| {
            m.keys()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut taken = declared.clone();
    for (svc, i, target) in anonymous {
        if !matches!(before, Before::New) {
            notes.push(format!(
                "service '{svc}' anonymous volume at {target} cannot carry labels; not converted on an existing stack (it would mount a new, empty volume)"
            ));
            continue;
        }
        let volume = format!("{svc}_{}", slug(&target));
        if !taken.insert(volume.clone()) {
            notes.push(format!(
                "service '{svc}' anonymous volume at {target} not converted: volume '{volume}' is already declared"
            ));
            continue;
        }
        if let Some(entry) = doc
            .get_mut("services")
            .and_then(|s| s.get_mut(svc.as_str()))
            .and_then(|s| s.get_mut("volumes"))
            .and_then(|v| v.get_mut(i))
        {
            match entry {
                Value::Mapping(m) => {
                    m.insert(Value::from("source"), Value::from(volume.as_str()));
                }
                other => *other = Value::from(format!("{volume}:{target}")),
            }
        }
        conversions.push((volume, svc, target));
    }

    for key in &declared {
        let entry = doc.get("volumes").and_then(|v| v.get(key.as_str()));
        if is_external(entry) {
            continue;
        }
        let wanted = match mounts.get(key).map(Vec::as_slice) {
            Some([(svc, target)]) => {
                Labels::for_(OWNER_DOCKGE, &project, Some(svc), Some(stack)).with_mount(target)
            }
            _ => Labels::for_(OWNER_DOCKGE, &project, None, Some(stack)),
        };
        let labels = match before.prior("volumes", key) {
            Prior::Absent => wanted.to_map(),
            Prior::Managed(have) => have,
            Prior::Unlabeled => {
                notes.push(format!(
                    "volume '{key}' was deployed without orca labels; not relabeled (compose would recreate it)"
                ));
                continue;
            }
        };
        if let Some(entry) = doc.get_mut("volumes").and_then(|v| v.get_mut(key.as_str())) {
            merge_labels(entry, &labels);
        }
    }

    if !conversions.is_empty() {
        let root = doc
            .as_mapping_mut()
            .context("compose root is not a mapping")?;
        let volumes = root
            .entry(Value::from("volumes"))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if !volumes.is_mapping() {
            *volumes = Value::Mapping(Mapping::new());
        }
        if let Some(volumes) = volumes.as_mapping_mut() {
            for (volume, svc, target) in conversions {
                let mut entry = Mapping::new();
                entry.insert(
                    Value::from("name"),
                    Value::from(format!("{project}_{volume}")),
                );
                let mut entry = Value::Mapping(entry);
                merge_labels(
                    &mut entry,
                    &Labels::for_(OWNER_DOCKGE, &project, Some(&svc), Some(stack))
                        .with_mount(&target)
                        .to_map(),
                );
                volumes.insert(Value::from(volume), entry);
            }
        }
    }

    let mut networks: BTreeSet<String> = doc
        .get("networks")
        .and_then(Value::as_mapping)
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
        if is_external(doc.get("networks").and_then(|n| n.get(key.as_str()))) {
            continue;
        }
        let labels = match before.prior("networks", &key) {
            Prior::Absent => Labels::for_(OWNER_DOCKGE, &project, None, Some(stack)).to_map(),
            Prior::Managed(have) => have,
            Prior::Unlabeled => {
                notes.push(format!(
                    "network '{key}' was deployed without orca labels; not relabeled (compose would recreate it)"
                ));
                continue;
            }
        };
        let root = doc
            .as_mapping_mut()
            .context("compose root is not a mapping")?;
        let section = root
            .entry(Value::from("networks"))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if !section.is_mapping() {
            *section = Value::Mapping(Mapping::new());
        }
        if let Some(section) = section.as_mapping_mut() {
            let entry = section
                .entry(Value::from(key.as_str()))
                .or_insert(Value::Null);
            merge_labels(entry, &labels);
        }
    }

    let yaml = if doc == original {
        compose_yaml.to_string()
    } else {
        serde_yaml::to_string(&doc)?
    };
    Ok(Labeled { yaml, notes })
}

/// What `compose_yaml` leaves without orca's labels.
pub fn audit(compose_yaml: &str) -> Result<Coverage> {
    let doc = parse(compose_yaml)?;
    let mut out = Coverage::default();
    let Some(services) = doc.get("services").and_then(Value::as_mapping) else {
        return Ok(out);
    };
    let mut default_used = false;
    for (key, spec) in services {
        let Some(svc) = key.as_str() else {
            continue;
        };
        default_used |= uses_default_network(spec);
        if !labels::is_managed(own_or_inherited_labels(Some(spec)).iter()) {
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
                !is_external(entry) && !labels::is_managed(own_or_inherited_labels(entry).iter())
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
    fn diff_marks_added_and_removed_lines() {
        assert_eq!(diff("a\nb\n", "a\nb\n"), "");
        assert_eq!(diff("a\nb\nc\n", "a\nx\nc\n"), " a\n-b\n+x\n c\n");
    }
}
