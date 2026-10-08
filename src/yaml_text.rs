//! Minimal line edits to block-style YAML text, so the comments, anchors and
//! quoting an operator wrote survive. Re-serializing a parsed document would
//! drop comments, expand anchors, and unquote strings like `'1_000'` that
//! compose then reads as numbers.
//!
//! Only block style is edited. Anything this cannot place unambiguously
//! (flow style, anchors or aliases on an edited node, block scalars) is
//! refused with a reason. Callers re-parse the result and compare it with the
//! document they intended, so a misplaced edit is caught, never deployed.

use std::collections::BTreeMap;

use plugin_toolkit::serde_json;
use serde_yaml::{Mapping, Value};

/// Why an edit was refused.
pub type Refusal = String;

#[derive(Debug, Clone)]
pub struct Text {
    lines: Vec<String>,
    /// Non-blank lines inside a block scalar: text, even when they look like
    /// a comment or a key.
    body: Vec<bool>,
    trailing_newline: bool,
    /// Indent step for new nested lines, taken from the `services` block.
    unit: usize,
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

fn is_content(l: &str) -> bool {
    let t = l.trim();
    !t.is_empty() && !t.starts_with('#')
}

fn is_directive(l: &str) -> bool {
    l.starts_with('%')
}

fn is_item(l: &str) -> bool {
    let t = l.trim_start();
    t == "-" || t.starts_with("- ")
}

fn sp(n: usize) -> String {
    " ".repeat(n)
}

/// `rest` with a trailing ` # comment` removed. Quoted values are returned
/// whole; callers only inspect unquoted ones.
fn strip_comment(rest: &str) -> &str {
    if rest.starts_with('#') {
        return "";
    }
    if rest.starts_with(['"', '\'']) {
        return rest;
    }
    rest.match_indices([' ', '\t'])
        .map(|(i, _)| i)
        .find(|&i| rest[i + 1..].starts_with('#'))
        .map_or(rest, |i| &rest[..i])
        .trim_end()
}

/// `(key, rest)` of a block mapping entry; `rest` is the value text on the
/// key's line, comment removed.
fn entry(text: &str) -> Option<(String, String)> {
    let t = text.trim_start();
    if t.is_empty() || is_item(t) || t.starts_with(['#', '?', '{', '[', '&', '*', '!', '|', '>']) {
        return None;
    }
    let (key, after) = match t.chars().next() {
        Some(q @ ('"' | '\'')) => {
            let close = t[1..].find(q)? + 1;
            (t[1..close].to_string(), &t[close + 1..])
        }
        _ => {
            let colon = t
                .match_indices(':')
                .map(|(i, _)| i)
                .find(|&i| t[i + 1..].is_empty() || t[i + 1..].starts_with([' ', '\t']))?;
            (t[..colon].trim_end().to_string(), &t[colon..])
        }
    };
    let rest = after.trim_start().strip_prefix(':')?;
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    Some((key, strip_comment(rest.trim()).to_string()))
}

/// The text after an item's dash.
fn after_dash(l: &str) -> &str {
    let t = l.trim_start();
    t.strip_prefix('-').unwrap_or(t)
}

/// The column a block scalar starting on `l` must be indented past, or
/// `None` when `l` does not start one.
fn scalar_header(l: &str) -> Option<usize> {
    let mut pos = indent(l);
    let mut col = pos;
    let mut t = &l[pos..];
    while is_item(t) {
        col = pos;
        let rest = t[1..].trim_start();
        pos += t.len() - rest.len();
        t = rest;
    }
    let value = match entry(t) {
        Some((_, r)) => {
            col = pos;
            r
        }
        None => strip_comment(t).to_string(),
    };
    value.starts_with(['|', '>']).then_some(col)
}

fn scan_body(lines: &[String]) -> Vec<bool> {
    let mut body = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let Some(col) = scalar_header(&lines[i]) else {
            i += 1;
            continue;
        };
        i += 1;
        while i < lines.len() {
            let l = &lines[i];
            if !l.trim().is_empty() {
                if indent(l) <= col {
                    break;
                }
                body[i] = true;
            }
            i += 1;
        }
    }
    body
}

fn describe(rest: &str) -> Refusal {
    match rest.chars().next() {
        Some('&') => "it carries a YAML anchor".into(),
        Some('*') => "it is a YAML alias".into(),
        Some('{' | '[') => "it is in flow style".into(),
        Some('|' | '>') => "it is a block scalar".into(),
        _ => "it is not a block mapping".into(),
    }
}

/// A key as written: plain when it cannot read as anything but a string.
pub fn key(k: &str) -> String {
    let plain = k.starts_with(|c: char| c.is_ascii_alphabetic())
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
        && !matches!(
            k.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n" | "null"
        );
    if plain { k.to_string() } else { quote(k) }
}

/// A double-quoted string scalar; JSON escapes are valid YAML.
pub fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// Block lines for `m` at column `col`. Strings are quoted.
pub fn render_map(m: &Mapping, col: usize, unit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in m {
        let k = key(k.as_str().unwrap_or_default());
        match v {
            Value::Mapping(inner) => {
                out.push(format!("{}{k}:", sp(col)));
                out.extend(render_map(inner, col + unit, unit));
            }
            Value::String(s) => out.push(format!("{}{k}: {}", sp(col), quote(s))),
            Value::Bool(b) => out.push(format!("{}{k}: {b}", sp(col))),
            Value::Number(n) => out.push(format!("{}{k}: {n}", sp(col))),
            _ => out.push(format!("{}{k}:", sp(col))),
        }
    }
    out
}

impl Text {
    pub fn new(src: &str) -> Result<Self, Refusal> {
        if src.contains('\r') {
            return Err("it has CRLF line endings".into());
        }
        let lines: Vec<String> = src.lines().map(str::to_string).collect();
        let mut t = Self {
            body: scan_body(&lines),
            lines,
            trailing_newline: src.is_empty() || src.ends_with('\n'),
            unit: 2,
        };
        let mut first = true;
        for i in (0..t.lines.len()).filter(|&i| t.content(i) && !t.body[i]) {
            let l = &t.lines[i];
            let s = l.trim_end();
            if first && is_directive(s) {
                continue;
            }
            if (s.starts_with("---") && !first) || s == "..." {
                return Err("it holds more than one YAML document".into());
            }
            if !first && indent(l) == 0 && entry(l).is_none() {
                return Err("its root is not a block mapping".into());
            }
            first = false;
        }
        if let Some((c, _)) = t.find(None, "services").and_then(|s| t.kids(Some(s)))
            && c > 0
        {
            t.unit = c;
        }
        Ok(t)
    }

    pub fn unit(&self) -> usize {
        self.unit
    }

    pub fn render(&self) -> String {
        let mut s = self.lines.join("\n");
        if self.trailing_newline && !self.lines.is_empty() {
            s.push('\n');
        }
        s
    }

    /// `Err` unless the rendered text parses to `doc`; an edit is only kept
    /// when it does.
    pub fn reads_back_as(&self, doc: &Value) -> Result<(), Refusal> {
        let text = self.render();
        #[cfg(test)]
        let text = sabotage::apply(text);
        match serde_yaml::from_str::<Value>(&text) {
            Ok(v) if &v == doc => Ok(()),
            _ => Err("the edited compose did not read back as intended".into()),
        }
    }

    fn content(&self, i: usize) -> bool {
        self.body[i] || is_content(&self.lines[i])
    }

    /// A `---` or `%` directive line before the root mapping.
    fn is_header(&self, i: usize) -> bool {
        let l = &self.lines[i];
        (l.starts_with("---") || is_directive(l))
            && !(0..i).any(|j| self.content(j) && !is_directive(&self.lines[j]))
    }

    fn splice(&mut self, range: std::ops::Range<usize>, lines: Vec<String>) {
        self.lines.splice(range, lines);
        self.body = scan_body(&self.lines);
    }

    /// Last line of the node starting at line `at`.
    fn end(&self, at: usize) -> usize {
        let k = indent(&self.lines[at]);
        let key_line = !is_item(&self.lines[at]);
        let mut end = at;
        let mut compact = None;
        for i in at + 1..self.lines.len() {
            if !self.content(i) {
                continue;
            }
            let l = &self.lines[i];
            let ind = indent(l);
            let c = *compact.get_or_insert(key_line && ind == k && is_item(l));
            if ind > k || (c && ind == k && is_item(l)) {
                end = i;
            } else {
                break;
            }
        }
        end
    }

    /// Indent and lines of the direct children of `node` (`None` = root).
    fn kids(&self, node: Option<usize>) -> Option<(usize, Vec<usize>)> {
        let (start, stop) = match node {
            None => (0, self.lines.len()),
            Some(at) => (at + 1, self.end(at) + 1),
        };
        let content = |i: &usize| self.content(*i) && !self.is_header(*i);
        let first = (start..stop).find(content)?;
        let c = indent(&self.lines[first]);
        Some((
            c,
            (start..stop)
                .filter(content)
                .filter(|&i| indent(&self.lines[i]) == c)
                .collect(),
        ))
    }

    fn find(&self, node: Option<usize>, k: &str) -> Option<usize> {
        self.kids(node)?.1.into_iter().find(|&i| {
            !is_item(&self.lines[i]) && entry(&self.lines[i]).is_some_and(|(e, _)| e == k)
        })
    }

    /// The line of the node at `keys` from the root.
    pub fn path(&self, keys: &[&str]) -> Result<usize, Refusal> {
        let mut node = None;
        for k in keys {
            node = Some(
                self.find(node, k)
                    .ok_or_else(|| format!("'{k}' is not in block style"))?,
            );
        }
        node.ok_or_else(|| "empty path".into())
    }

    fn rest(&self, at: usize) -> String {
        entry(&self.lines[at]).map(|(_, r)| r).unwrap_or_default()
    }

    /// Make the key line `at` ready for block children: an empty `{}` or
    /// `null` value is dropped; anything else is refused.
    fn open(&mut self, at: usize) -> Result<(), Refusal> {
        let (k, rest) = entry(&self.lines[at]).ok_or("it is not a mapping entry")?;
        match rest.as_str() {
            "" => Ok(()),
            "{}" | "null" | "~" => {
                self.lines[at] = format!("{}{}:", sp(indent(&self.lines[at])), key(&k));
                Ok(())
            }
            r => Err(describe(r)),
        }
    }

    fn child_indent(&self, at: usize) -> usize {
        self.kids(Some(at))
            .map_or(indent(&self.lines[at]) + self.unit, |(c, _)| c)
    }

    fn insert_after(&mut self, at: usize, lines: Vec<String>) {
        let at = at + 1;
        self.splice(at..at, lines);
    }

    /// Set `wanted` in the `labels` of the node at `at`. `current` is its own
    /// labels, `None` when it has no `labels` key; `seed` is then written
    /// first (labels a `<<` merge key would have given it).
    pub fn set_labels(
        &mut self,
        at: usize,
        wanted: &BTreeMap<String, String>,
        current: Option<&BTreeMap<String, String>>,
        seed: &BTreeMap<String, String>,
    ) -> Result<(), Refusal> {
        self.open(at)?;
        let Some(l) = self.find(Some(at), "labels") else {
            let c = self.child_indent(at);
            let mut all = seed.clone();
            all.extend(wanted.clone());
            let mut lines = vec![format!("{}labels:", sp(c))];
            lines.extend(
                all.iter()
                    .map(|(k, v)| format!("{}{}: {}", sp(c + self.unit), key(k), quote(v))),
            );
            let e = self.end(at);
            self.insert_after(e, lines);
            return Ok(());
        };
        self.open(l)
            .map_err(|why| format!("its labels cannot be edited: {why}"))?;
        let current = current.cloned().unwrap_or_default();
        let (ec, list) = match self.kids(Some(l)) {
            Some((c, ks)) => (c, is_item(&self.lines[ks[0]])),
            None => (indent(&self.lines[l]) + self.unit, false),
        };
        for (k, v) in wanted {
            if current.get(k) == Some(v) {
                continue;
            }
            let ks = self.kids(Some(l)).map(|(_, ks)| ks).unwrap_or_default();
            let existing = ks.into_iter().find(|&i| {
                let line = &self.lines[i];
                if list {
                    is_item(line) && list_label_key(after_dash(line)).as_deref() == Some(k)
                } else {
                    !is_item(line) && entry(line).is_some_and(|(e, _)| &e == k)
                }
            });
            let line = if list {
                format!("{}- {}", sp(ec), quote(&format!("{k}={v}")))
            } else {
                format!("{}{}: {}", sp(ec), key(k), quote(v))
            };
            match existing {
                Some(i) => {
                    let e = self.end(i);
                    self.splice(i..e + 1, vec![line]);
                }
                None => {
                    let e = self.end(l);
                    self.insert_after(e, vec![line]);
                }
            }
        }
        Ok(())
    }

    /// Append `key:` with `body` under root section `section`, creating the
    /// section when absent.
    pub fn add_entry(&mut self, section: &str, k: &str, body: &Mapping) -> Result<(), Refusal> {
        let sec = match self.find(None, section) {
            Some(l) => {
                self.open(l)
                    .map_err(|why| format!("top-level '{section}': {why}"))?;
                l
            }
            None => {
                let last = (0..self.lines.len())
                    .rev()
                    .find(|&i| self.content(i))
                    .ok_or("the file is empty")?;
                self.insert_after(last, vec![format!("{section}:")]);
                last + 1
            }
        };
        let c = self.child_indent(sec);
        let mut lines = vec![format!("{}{}:", sp(c), key(k))];
        lines.extend(render_map(body, c + self.unit, self.unit));
        let e = self.end(sec);
        self.insert_after(e, lines);
        Ok(())
    }

    /// The line of item `i` of the block sequence `k` under the node at `at`.
    pub fn seq_item(&self, at: usize, k: &str, i: usize) -> Result<usize, Refusal> {
        let l = self
            .find(Some(at), k)
            .ok_or_else(|| format!("'{k}' is not in block style"))?;
        let rest = self.rest(l);
        if !rest.is_empty() {
            return Err(format!("'{k}': {}", describe(&rest)));
        }
        let (_, ks) = self
            .kids(Some(l))
            .ok_or_else(|| format!("'{k}' is empty"))?;
        if !ks.iter().all(|&j| is_item(&self.lines[j])) {
            return Err(format!("'{k}' is not a block sequence"));
        }
        ks.get(i)
            .copied()
            .ok_or_else(|| format!("'{k}' item {i} not found"))
    }

    /// Replace a one-line item with `lines`.
    pub fn replace_item(&mut self, item: usize, lines: Vec<String>) -> Result<(), Refusal> {
        let rest = after_dash(&self.lines[item]).trim_start();
        if self.end(item) != item || rest.starts_with(['&', '*', '{', '[', '|', '>', '!']) {
            return Err("the item is not a plain one-line value".into());
        }
        self.splice(item..item + 1, lines);
        Ok(())
    }

    pub fn item_indent(&self, item: usize) -> usize {
        indent(&self.lines[item])
    }

    /// Column of the keys of the block mapping that item `item` holds.
    fn item_col(&self, item: usize) -> Option<usize> {
        let l = &self.lines[item];
        let after = &l[indent(l) + 1..];
        let t = after.trim_start();
        if t.is_empty() || t.starts_with('#') {
            let e = self.end(item);
            return (item + 1..=e)
                .find(|&j| self.content(j))
                .filter(|&j| !self.body[j] && entry(&self.lines[j]).is_some())
                .map(|j| indent(&self.lines[j]));
        }
        entry(t).map(|_| indent(l) + 1 + after.len() - t.len())
    }

    /// The line of key `k` in the mapping item `item`.
    fn item_key(&self, item: usize, k: &str) -> Option<usize> {
        let col = self.item_col(item)?;
        (item..=self.end(item)).find(|&j| {
            let line = &self.lines[j];
            let text = if j == item {
                after_dash(line)
            } else if indent(line) == col && !is_item(line) {
                line.as_str()
            } else {
                return false;
            };
            entry(text).is_some_and(|(e, _)| e == k)
        })
    }

    /// Append `lines` (relative indent 0) to the mapping item `item`.
    pub fn item_append(&mut self, item: usize, lines: &[String]) -> Result<(), Refusal> {
        let col = self
            .item_col(item)
            .ok_or("the item is not a block mapping")?;
        let e = self.end(item);
        self.insert_after(e, lines.iter().map(|l| format!("{}{l}", sp(col))).collect());
        Ok(())
    }

    /// Append `lines` (relative indent 0) to the mapping under key `k` of the
    /// mapping item `item`.
    pub fn item_child_append(
        &mut self,
        item: usize,
        k: &str,
        lines: &[String],
    ) -> Result<(), Refusal> {
        let at = self
            .item_key(item, k)
            .ok_or_else(|| format!("'{k}' not found"))?;
        if at == item {
            return Err(format!("'{k}' shares the item's first line"));
        }
        self.open(at)?;
        let c = self.child_indent(at);
        let e = self.end(at);
        self.insert_after(e, lines.iter().map(|l| format!("{}{l}", sp(c))).collect());
        Ok(())
    }
}

/// Forces [`Text::reads_back_as`] to fail, so tests reach the fallback that
/// a misplaced edit takes.
#[cfg(test)]
pub(crate) mod sabotage {
    use std::cell::RefCell;

    thread_local! {
        static WHEN: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    /// Fail read-back of any text containing `when`, on this thread.
    pub fn arm(when: Option<&str>) {
        WHEN.with(|w| *w.borrow_mut() = when.map(str::to_string));
    }

    pub(super) fn apply(text: String) -> String {
        match WHEN.with(|w| w.borrow().clone()) {
            Some(when) if text.contains(&when) => text + "x-sabotage: true\n",
            _ => text,
        }
    }
}

/// The label key of a list-form label item (`k=v`).
fn list_label_key(item: &str) -> Option<String> {
    let s: String = serde_yaml::from_str(strip_comment(item.trim())).ok()?;
    Some(s.split_once('=').map_or(s.as_str(), |(k, _)| k).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_reads_keys_and_values() {
        assert_eq!(entry("  app:"), Some(("app".into(), "".into())));
        assert_eq!(entry("a: {} # x"), Some(("a".into(), "{}".into())));
        assert_eq!(entry("\"a b\": &x"), Some(("a b".into(), "&x".into())));
        assert_eq!(entry("- a: b"), None);
        assert_eq!(entry("http://x"), None);
    }

    #[test]
    fn keys_that_would_not_read_as_strings_are_quoted() {
        assert_eq!(key("orca.managed"), "orca.managed");
        assert_eq!(key("yes"), "\"yes\"");
        assert_eq!(key("1x"), "\"1x\"");
    }

    #[test]
    fn multi_document_files_are_refused() {
        assert!(Text::new("---\na: 1\n").is_ok());
        assert!(Text::new("a: 1\n---\nb: 2\n").is_err());
    }

    #[test]
    fn a_yaml_directive_before_the_document_is_allowed() {
        let t = Text::new("%YAML 1.2\n---\na:\n  b: 1\n").unwrap();
        assert_eq!(t.path(&["a", "b"]), Ok(3));
    }

    #[test]
    fn tabs_separate_values_and_comments() {
        assert_eq!(entry("labels:\t# c"), Some(("labels".into(), "".into())));
        assert_eq!(entry("image:\tx\t# c"), Some(("image".into(), "x".into())));
        assert_eq!(strip_comment("x\t#c"), "x");
        assert_eq!(strip_comment("a#b"), "a#b");
    }

    #[test]
    fn hash_lines_inside_a_block_scalar_are_text() {
        let src = "a:\n  cmd: |\n    echo\n    # kept\nb:\n  - |-\n    x\n    # also\n  - y\n";
        assert!(serde_yaml::from_str::<Value>(src).is_ok());
        let t = Text::new(src).unwrap();
        assert_eq!(t.end(0), 3);
        assert_eq!(t.end(4), 8);
        assert!(t.body[3] && t.body[7] && !t.body[8]);
    }
}
