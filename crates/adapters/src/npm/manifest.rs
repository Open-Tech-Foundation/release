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
    #[cfg(test)]
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

    pub(crate) fn json(&self) -> Result<Value> {
        let cleaned = strip_jsonc_comments(&self.content);
        serde_json::from_str(&cleaned).with_context(|| format!("parsing {}", self.path.display()))
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

    /// The `scripts.<name>` command string, if present.
    pub fn script(&self, name: &str) -> Option<String> {
        self.get_string(&["scripts", name])
    }

    /// Remove the object entry at `path` (both its key and value), fixing the adjacent comma and
    /// preserving all other bytes. Returns `true` if the key existed and was removed. Only object
    /// keys are navigated — exactly what manifest edits need.
    pub fn remove_key(&mut self, path: &[&str]) -> Result<bool> {
        let Some((key, parent)) = path.split_last() else {
            return Ok(false);
        };
        let b = self.content.as_bytes();
        // Find the opening brace of the object that *contains* `key`.
        let obj_start = if parent.is_empty() {
            let mut i = 0;
            skip_ws(b, &mut i);
            if b.get(i) != Some(&b'{') {
                return Ok(false);
            }
            i
        } else {
            match locate_value(&self.content, parent) {
                Some(span) if b.get(span.start) == Some(&b'{') => span.start,
                _ => return Ok(false),
            }
        };
        let entries = object_entries(&self.content, obj_start)?;
        let Some(idx) = entries.iter().position(|e| e.key == *key) else {
            return Ok(false);
        };
        // Remove the entry plus exactly one adjacent comma, using entry boundaries so the
        // surviving neighbour keeps its original leading whitespace/indentation.
        let n = entries.len();
        let (start, end) = if n == 1 {
            (entries[0].key_start, entries[0].value_end)
        } else if idx < n - 1 {
            (entries[idx].key_start, entries[idx + 1].key_start)
        } else {
            (entries[idx - 1].value_end, entries[idx].value_end)
        };
        let mut next = String::with_capacity(self.content.len());
        next.push_str(&self.content[..start]);
        next.push_str(&self.content[end..]);
        self.content = next;
        Ok(true)
    }
}

/// One `"key": value` entry located inside an object, with the byte offsets that bound it.
struct ObjectEntry {
    key: String,
    /// Offset of the key's opening quote.
    key_start: usize,
    /// Offset just past the entry's value (before any trailing whitespace or comma).
    value_end: usize,
}

/// Collect the direct entries of the object whose opening `{` is at `obj_start`.
fn object_entries(s: &str, obj_start: usize) -> Result<Vec<ObjectEntry>> {
    let b = s.as_bytes();
    let mut i = obj_start;
    if b.get(i) != Some(&b'{') {
        return Err(anyhow!("expected an object at byte {obj_start}"));
    }
    i += 1;
    let mut entries = Vec::new();
    loop {
        skip_ws(b, &mut i);
        match b.get(i) {
            Some(b'"') => {}
            _ => break, // '}' (end) or malformed — stop collecting.
        }
        let key_start = i;
        scan_string(b, &mut i).ok_or_else(|| anyhow!("unterminated key"))?;
        let key = serde_json::from_str::<String>(&s[key_start..i])?;
        skip_ws(b, &mut i);
        if b.get(i) != Some(&b':') {
            return Err(anyhow!("expected ':' after key"));
        }
        i += 1;
        skip_ws(b, &mut i);
        scan_value(b, &mut i).ok_or_else(|| anyhow!("bad value for {key}"))?;
        entries.push(ObjectEntry {
            key,
            key_start,
            value_end: i,
        });
        skip_ws(b, &mut i);
        match b.get(i) {
            Some(b',') => i += 1,
            _ => break,
        }
    }
    Ok(entries)
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

pub(crate) fn strip_jsonc_comments(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_string = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut chars = input.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if in_line_comment {
            if c == '\n' || c == '\r' {
                in_line_comment = false;
                output.push(c);
            }
            continue;
        }
        if in_block_comment {
            if c == '*' {
                if let Some((_, '/')) = chars.peek() {
                    chars.next();
                    in_block_comment = false;
                }
            }
            continue;
        }
        if in_string {
            output.push(c);
            if c == '"' {
                let mut backslashes = 0;
                let mut j = i;
                let bytes = input.as_bytes();
                while j > 0 && bytes[j - 1] == b'\\' {
                    backslashes += 1;
                    j -= 1;
                }
                if backslashes % 2 == 0 {
                    in_string = false;
                }
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            output.push(c);
        } else if c == '/' {
            if let Some((_, '/')) = chars.peek() {
                chars.next();
                in_line_comment = true;
            } else if let Some((_, '*')) = chars.peek() {
                chars.next();
                in_block_comment = true;
            } else {
                output.push(c);
            }
        } else {
            output.push(c);
        }
    }
    output
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
    fn reads_a_build_script() {
        let src = r#"{
  "name": "@x/a",
  "scripts": { "build": "tsc -p .", "test": "vitest" }
}
"#;
        let man = m(src);
        assert_eq!(man.script("build").unwrap(), "tsc -p .");
        assert_eq!(man.script("prepack"), None);
    }

    #[test]
    fn remove_middle_key_keeps_siblings_and_indent() {
        let src = "{\n  \"scripts\": {\n    \"build\": \"tsc\",\n    \"prepack\": \"npm run build\",\n    \"test\": \"vitest\"\n  }\n}\n";
        let mut man = m(src);
        assert!(man.remove_key(&["scripts", "prepack"]).unwrap());
        assert_eq!(
            man.content(),
            "{\n  \"scripts\": {\n    \"build\": \"tsc\",\n    \"test\": \"vitest\"\n  }\n}\n"
        );
    }

    #[test]
    fn remove_last_key_drops_preceding_comma() {
        let src = "{\n  \"scripts\": {\n    \"build\": \"tsc\",\n    \"prepack\": \"npm run build\"\n  }\n}\n";
        let mut man = m(src);
        assert!(man.remove_key(&["scripts", "prepack"]).unwrap());
        assert_eq!(
            man.content(),
            "{\n  \"scripts\": {\n    \"build\": \"tsc\"\n  }\n}\n"
        );
    }

    #[test]
    fn remove_only_key_empties_the_object() {
        let src = "{\n  \"scripts\": {\n    \"prepack\": \"npm run build\"\n  }\n}\n";
        let mut man = m(src);
        assert!(man.remove_key(&["scripts", "prepack"]).unwrap());
        assert_eq!(man.content(), "{\n  \"scripts\": {\n    \n  }\n}\n");
    }

    #[test]
    fn remove_missing_key_reports_false_and_leaves_content() {
        let src = "{\n  \"scripts\": { \"build\": \"tsc\" }\n}\n";
        let mut man = m(src);
        assert!(!man.remove_key(&["scripts", "prepack"]).unwrap());
        assert_eq!(man.content(), src);
        // Missing parent object is also a no-op, not an error.
        assert!(!man.remove_key(&["missing", "x"]).unwrap());
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

    #[test]
    fn test_strip_jsonc_comments() {
        let jsonc = r#"{
            // line comment
            "name": "foo", /* block comment */
            "url": "https://foo.bar", // containing slash
            "escaped": "foo \" // bar"
        }"#;
        let cleaned = strip_jsonc_comments(jsonc);
        assert!(!cleaned.contains("// line comment"));
        assert!(!cleaned.contains("/* block comment */"));
        assert!(cleaned.contains("https://foo.bar"));
        assert!(cleaned.contains(r#"foo \" // bar"#));
        let parsed: serde_json::Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(parsed["name"], "foo");
        assert_eq!(parsed["url"], "https://foo.bar");
    }
}
