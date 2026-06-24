//! Format-preserving `package.json` reading and editing.
//!
//! Reads happen via `serde_json` ([`Manifest::json`]); writes are **surgical** — a small
//! JSON span locator finds the exact byte range of the value to change and splices in the
//! new value, leaving every other byte (key order, indentation, trailing newline, comments
//! npm tolerates) untouched. This is what keeps manifest writes byte-stable except for the
//! intended change.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// The dependency sections we treat as carrying internal edges, in a stable order.
pub const DEP_SECTIONS: [&str; 4] = [
    "dependencies",
    "peerDependencies",
    "devDependencies",
    "optionalDependencies",
];

/// One declared dependency, with the section it came from.
#[derive(Debug, Clone)]
pub struct DepRecord {
    pub section: &'static str,
    pub name: String,
    pub range: String,
}

/// A `package.json` held as its raw text, edited in place without reformatting.
pub struct Manifest {
    pub path: PathBuf,
    content: String,
}

impl Manifest {
    pub fn read(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("reading manifest {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            content,
        })
    }

    /// Construct from in-memory content (used in tests).
    pub fn new(path: PathBuf, content: String) -> Self {
        Self { path, content }
    }

    /// The current raw text.
    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn save(&self) -> Result<()> {
        fs::write(&self.path, &self.content)
            .with_context(|| format!("writing manifest {}", self.path.display()))
    }

    fn json(&self) -> Result<Value> {
        serde_json::from_str(&self.content)
            .with_context(|| format!("parsing {}", self.path.display()))
    }

    pub fn name(&self) -> Result<String> {
        self.get_string(&["name"])
            .ok_or_else(|| anyhow!("{}: missing \"name\"", self.path.display()))
    }

    pub fn version(&self) -> Result<String> {
        self.get_string(&["version"])
            .ok_or_else(|| anyhow!("{}: missing \"version\"", self.path.display()))
    }

    pub fn is_private(&self) -> bool {
        self.json()
            .ok()
            .and_then(|j| j.get("private").and_then(Value::as_bool))
            == Some(true)
    }

    /// Every declared dependency across [`DEP_SECTIONS`] whose value is a string range.
    pub fn deps(&self) -> Result<Vec<DepRecord>> {
        let json = self.json()?;
        let mut out = Vec::new();
        for section in DEP_SECTIONS {
            if let Some(map) = json.get(section).and_then(Value::as_object) {
                for (name, val) in map {
                    if let Some(range) = val.as_str() {
                        out.push(DepRecord {
                            section,
                            name: name.clone(),
                            range: range.to_string(),
                        });
                    }
                }
            }
        }
        Ok(out)
    }

    /// Read the string value at an object-key `path` (e.g. `["dependencies", "@x/a"]`).
    pub fn get_string(&self, path: &[&str]) -> Option<String> {
        let span = locate_value(&self.content, path)?;
        serde_json::from_str::<String>(&self.content[span]).ok()
    }

    /// Replace the string value at `path` with `new`, preserving all surrounding bytes.
    /// Returns `true` if the path existed (and was rewritten), `false` otherwise.
    pub fn set_string(&mut self, path: &[&str], new: &str) -> Result<bool> {
        let Some(span) = locate_value(&self.content, path) else {
            return Ok(false);
        };
        let encoded = serde_json::to_string(new)?; // a quoted JSON string
        let mut next = String::with_capacity(self.content.len() + encoded.len());
        next.push_str(&self.content[..span.start]);
        next.push_str(&encoded);
        next.push_str(&self.content[span.end..]);
        self.content = next;
        Ok(true)
    }
}

// ---- minimal JSON span locator -------------------------------------------------------------
//
// Navigates nested object keys and returns the byte span of the located value (quotes
// included for strings). It only walks object keys — exactly what manifest edits need — and
// skips over any other value shape it encounters.

fn locate_value(s: &str, path: &[&str]) -> Option<Range<usize>> {
    if path.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    let mut i = 0;
    skip_ws(b, &mut i);
    if b.get(i)? != &b'{' {
        return None;
    }
    locate_in_object(s, &mut i, path)
}

fn locate_in_object(s: &str, i: &mut usize, path: &[&str]) -> Option<Range<usize>> {
    let b = s.as_bytes();
    if b.get(*i)? != &b'{' {
        return None;
    }
    *i += 1;
    loop {
        skip_ws(b, i);
        match b.get(*i)? {
            b'}' => return None,
            b'"' => {}
            _ => return None,
        }
        let key_start = *i;
        scan_string(b, i)?;
        let key = serde_json::from_str::<String>(&s[key_start..*i]).ok()?;
        skip_ws(b, i);
        if b.get(*i)? != &b':' {
            return None;
        }
        *i += 1;
        skip_ws(b, i);
        let value_start = *i;

        if key == path[0] {
            if path.len() == 1 {
                scan_value(b, i)?;
                return Some(value_start..*i);
            }
            // Need to descend; the value must be an object.
            if b.get(*i)? != &b'{' {
                return None;
            }
            return locate_in_object(s, i, &path[1..]);
        }

        scan_value(b, i)?;
        skip_ws(b, i);
        match b.get(*i)? {
            b',' => {
                *i += 1;
                continue;
            }
            b'}' => return None,
            _ => return None,
        }
    }
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while matches!(b.get(*i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        *i += 1;
    }
}

/// `i` is at the opening quote; advances past the closing quote.
fn scan_string(b: &[u8], i: &mut usize) -> Option<()> {
    if b.get(*i)? != &b'"' {
        return None;
    }
    *i += 1;
    while let Some(&c) = b.get(*i) {
        match c {
            b'\\' => *i += 2,
            b'"' => {
                *i += 1;
                return Some(());
            }
            _ => *i += 1,
        }
    }
    None
}

/// Advances `i` past a complete JSON value (string, object, array, or scalar literal).
fn scan_value(b: &[u8], i: &mut usize) -> Option<()> {
    skip_ws(b, i);
    match b.get(*i)? {
        b'"' => scan_string(b, i),
        b'{' => scan_object(b, i),
        b'[' => scan_array(b, i),
        _ => {
            while !matches!(
                b.get(*i),
                None | Some(b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            ) {
                *i += 1;
            }
            Some(())
        }
    }
}

fn scan_object(b: &[u8], i: &mut usize) -> Option<()> {
    *i += 1; // consume '{'
    loop {
        skip_ws(b, i);
        match b.get(*i)? {
            b'}' => {
                *i += 1;
                return Some(());
            }
            b'"' => {}
            _ => return None,
        }
        scan_string(b, i)?;
        skip_ws(b, i);
        if b.get(*i)? != &b':' {
            return None;
        }
        *i += 1;
        scan_value(b, i)?;
        skip_ws(b, i);
        match b.get(*i)? {
            b',' => *i += 1,
            b'}' => {
                *i += 1;
                return Some(());
            }
            _ => return None,
        }
    }
}

fn scan_array(b: &[u8], i: &mut usize) -> Option<()> {
    *i += 1; // consume '['
    loop {
        skip_ws(b, i);
        if b.get(*i)? == &b']' {
            *i += 1;
            return Some(());
        }
        scan_value(b, i)?;
        skip_ws(b, i);
        match b.get(*i)? {
            b',' => *i += 1,
            b']' => {
                *i += 1;
                return Some(());
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(content: &str) -> Manifest {
        Manifest::new(PathBuf::from("package.json"), content.to_string())
    }

    #[test]
    fn reads_top_level_and_nested_strings() {
        let src = r#"{
  "name": "@x/a",
  "version": "1.2.3",
  "dependencies": { "@x/core": "^1.0.0", "left-pad": "~2.0.0" }
}
"#;
        let man = m(src);
        assert_eq!(man.get_string(&["name"]).unwrap(), "@x/a");
        assert_eq!(man.get_string(&["version"]).unwrap(), "1.2.3");
        assert_eq!(
            man.get_string(&["dependencies", "@x/core"]).unwrap(),
            "^1.0.0"
        );
        assert_eq!(man.get_string(&["dependencies", "missing"]), None);
    }

    #[test]
    fn set_version_is_byte_stable_except_target() {
        let src = "{\n  \"name\": \"@x/a\",\n  \"version\": \"1.0.0\"\n}\n";
        let mut man = m(src);
        assert!(man.set_string(&["version"], "2.0.0").unwrap());
        assert_eq!(
            man.content(),
            "{\n  \"name\": \"@x/a\",\n  \"version\": \"2.0.0\"\n}\n"
        );
    }

    #[test]
    fn set_nested_dep_range_leaves_siblings_untouched() {
        let src = r#"{
  "dependencies": { "@x/core": "^1.0.0", "@x/util": "^1.0.0" }
}
"#;
        let mut man = m(src);
        assert!(man
            .set_string(&["dependencies", "@x/core"], "^2.0.0")
            .unwrap());
        let expected = r#"{
  "dependencies": { "@x/core": "^2.0.0", "@x/util": "^1.0.0" }
}
"#;
        assert_eq!(man.content(), expected);
    }

    #[test]
    fn set_string_on_missing_path_reports_false() {
        let mut man = m("{\n  \"name\": \"@x/a\"\n}\n");
        assert!(!man.set_string(&["version"], "1.0.0").unwrap());
    }

    #[test]
    fn deps_lists_only_string_ranges_with_section() {
        let src = r#"{
  "dependencies": { "@x/core": "^1.0.0" },
  "peerDependencies": { "@x/sdk": "^2.0.0" }
}
"#;
        let recs = m(src).deps().unwrap();
        assert_eq!(recs.len(), 2);
        assert!(recs
            .iter()
            .any(|r| r.section == "peerDependencies" && r.name == "@x/sdk"));
    }
}
