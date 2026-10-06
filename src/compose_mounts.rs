//! Compose bind-mount propagation fix (orca #402 Part B).
//!
//! After a host CIFS/NFS remount, a new mount only propagates INTO a running
//! container if the bind carries `rslave` propagation (the container-side
//! complement to orca making its host mountpoints `rshared`). Without it the
//! container keeps the stale mount and hits ENOENT. So before dockge deploys a
//! stack we rewrite every **bind** volume to long syntax with
//! `bind.propagation: rslave` — never overriding an explicit propagation.
//!
//! The transform is deliberately conservative: anything it can't confidently
//! rewrite (unparseable YAML, unknown short-form flags, non-bind volumes) is
//! left exactly as-is so a deploy is never blocked by this pass.

use plugin_toolkit::anyhow::Result;
use serde_yaml::{Mapping, Value};

/// Ensure every bind mount in `compose_yaml` uses long syntax with
/// `bind.propagation: rslave`, unless a propagation is already set. Returns the
/// original text unchanged when there's nothing to do or it can't be parsed —
/// this must never fail a deploy.
pub fn ensure_bind_propagation(compose_yaml: &str) -> Result<String> {
    let mut doc: Value = match serde_yaml::from_str(compose_yaml) {
        Ok(v) => v,
        Err(_) => return Ok(compose_yaml.to_string()),
    };

    let Some(services) = doc.get_mut("services").and_then(Value::as_mapping_mut) else {
        return Ok(compose_yaml.to_string());
    };

    let mut changed = false;
    for (_svc, spec) in services.iter_mut() {
        let Some(volumes) = spec.get_mut("volumes").and_then(Value::as_sequence_mut) else {
            continue;
        };
        for entry in volumes.iter_mut() {
            if rewrite_volume(entry) {
                changed = true;
            }
        }
    }

    if !changed {
        return Ok(compose_yaml.to_string());
    }
    Ok(serde_yaml::to_string(&doc)?)
}

/// Rewrite one volume entry in place if it is a bind mount that lacks explicit
/// propagation. Returns whether the entry was modified.
fn rewrite_volume(entry: &mut Value) -> bool {
    match entry {
        // Short form `"<src>:<dst>[:opts]"` — a bind only if the source is a
        // host path. Convert to long syntax, preserving ro/rw + z/Z.
        Value::String(s) => {
            if let Some(long) = convert_short_bind(s) {
                *entry = Value::Mapping(long);
                true
            } else {
                false
            }
        }
        // Long form — only touch `type: bind` without an existing propagation.
        Value::Mapping(map) => ensure_long_bind_propagation(map),
        _ => false,
    }
}

/// True if `src` is a host path (bind), not a named volume.
pub(crate) fn is_host_path(src: &str) -> bool {
    src.starts_with('/') || src.starts_with("./") || src.starts_with("../") || src.starts_with('~')
}

/// Convert a short-form bind string to a long-syntax mapping with
/// `bind.propagation: rslave`. Returns `None` (leave unchanged) for named
/// volumes, malformed entries, or unknown option flags we can't faithfully map.
fn convert_short_bind(entry: &str) -> Option<Mapping> {
    let parts: Vec<&str> = entry.split(':').collect();
    // `src:dst` or `src:dst:opts` only; anything else is ambiguous — leave it.
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let src = parts[0];
    let dst = parts[1];
    if !is_host_path(src) || dst.is_empty() {
        return None;
    }

    let mut read_only = false;
    let mut selinux: Option<&str> = None;
    if let Some(opts) = parts.get(2) {
        for opt in opts.split(',') {
            match opt {
                "ro" => read_only = true,
                "rw" | "" => {}
                "z" | "Z" => selinux = Some(opt),
                // Unknown flag: converting would drop it — keep entry as-is.
                _ => return None,
            }
        }
    }

    let mut map = Mapping::new();
    map.insert(Value::from("type"), Value::from("bind"));
    map.insert(Value::from("source"), Value::from(src));
    map.insert(Value::from("target"), Value::from(dst));
    if read_only {
        map.insert(Value::from("read_only"), Value::from(true));
    }
    let mut bind = Mapping::new();
    bind.insert(Value::from("propagation"), Value::from("rslave"));
    if let Some(sel) = selinux {
        bind.insert(Value::from("selinux"), Value::from(sel));
    }
    map.insert(Value::from("bind"), Value::Mapping(bind));
    Some(map)
}

/// For a long-form `type: bind` entry, add `bind.propagation: rslave` when no
/// propagation is set. Returns whether the mapping was modified. Non-bind types
/// (volume/tmpfs/npipe) and entries with an explicit propagation are untouched.
fn ensure_long_bind_propagation(map: &mut Mapping) -> bool {
    if map.get("type").and_then(Value::as_str) != Some("bind") {
        return false;
    }
    match map.get("bind") {
        Some(Value::Mapping(b)) if b.contains_key("propagation") => false,
        Some(Value::Mapping(_)) => {
            if let Some(Value::Mapping(b)) = map.get_mut("bind") {
                b.insert(Value::from("propagation"), Value::from("rslave"));
                return true;
            }
            false
        }
        _ => {
            let mut bind = Mapping::new();
            bind.insert(Value::from("propagation"), Value::from("rslave"));
            map.insert(Value::from("bind"), Value::Mapping(bind));
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Value {
        serde_yaml::from_str(yaml).unwrap()
    }

    fn volume(yaml: &str, svc: &str, idx: usize) -> Value {
        parse(yaml)["services"][svc]["volumes"][idx].clone()
    }

    #[test]
    fn short_bind_becomes_long_with_rslave() {
        let yaml = "services:\n  app:\n    volumes:\n      - /srv/media:/data\n";
        let out = ensure_bind_propagation(yaml).unwrap();
        let v = volume(&out, "app", 0);
        assert_eq!(v["type"], Value::from("bind"));
        assert_eq!(v["source"], Value::from("/srv/media"));
        assert_eq!(v["target"], Value::from("/data"));
        assert_eq!(v["bind"]["propagation"], Value::from("rslave"));
    }

    #[test]
    fn short_bind_ro_preserves_read_only() {
        let yaml = "services:\n  app:\n    volumes:\n      - /srv/media:/data:ro\n";
        let out = ensure_bind_propagation(yaml).unwrap();
        let v = volume(&out, "app", 0);
        assert_eq!(v["read_only"], Value::from(true));
        assert_eq!(v["bind"]["propagation"], Value::from("rslave"));
    }

    #[test]
    fn named_volume_is_untouched() {
        let yaml = "services:\n  app:\n    volumes:\n      - appdata:/config\n";
        let out = ensure_bind_propagation(yaml).unwrap();
        assert_eq!(volume(&out, "app", 0), Value::from("appdata:/config"));
    }

    #[test]
    fn long_type_volume_is_untouched() {
        let yaml = "services:\n  app:\n    volumes:\n      - type: volume\n        source: appdata\n        target: /config\n";
        let out = ensure_bind_propagation(yaml).unwrap();
        let v = volume(&out, "app", 0);
        assert!(v.get("bind").is_none());
    }

    #[test]
    fn explicit_propagation_is_preserved() {
        let yaml = "services:\n  app:\n    volumes:\n      - type: bind\n        source: /srv/media\n        target: /data\n        bind:\n          propagation: shared\n";
        let out = ensure_bind_propagation(yaml).unwrap();
        let v = volume(&out, "app", 0);
        assert_eq!(v["bind"]["propagation"], Value::from("shared"));
    }

    #[test]
    fn idempotent() {
        let yaml =
            "services:\n  app:\n    volumes:\n      - /srv/media:/data\n      - appdata:/config\n";
        let once = ensure_bind_propagation(yaml).unwrap();
        let twice = ensure_bind_propagation(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn no_services_returned_unchanged() {
        let yaml = "version: \"3\"\n";
        assert_eq!(ensure_bind_propagation(yaml).unwrap(), yaml);
    }

    #[test]
    fn non_parsing_returned_unchanged() {
        let yaml = "this: : : not yaml\n  - broken";
        assert_eq!(ensure_bind_propagation(yaml).unwrap(), yaml);
    }
}
