//! The provenance stamp on a generated `release.yml`, and what it lets `upgrade` report.
//!
//! The workflow is generated from `release.toml`, and regenerating it replaces the file. A hand
//! edit made in the meantime — a workaround for something the config cannot yet say — used to be
//! dropped without a word. The first line of every generated workflow now carries a hash of what
//! was generated, so a later run can tell an untouched file from an edited one and say which lines
//! it is about to discard before it does.
//!
//! The stamp holds only a hash, not the original text, so the lines reported are the current
//! file's lines that the regenerated workflow does not contain. That includes the hand edits, and
//! may also include lines a newer generator writes differently.

use sha2::{Digest, Sha256};

const PREFIX: &str = "# Generated from release.toml by `release` · sha256:";

/// What the stamp says about a workflow file's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// No stamp: written by hand, or generated before stamps existed. Nothing can be inferred.
    Unstamped,
    /// Exactly what the generator wrote.
    Generated,
    /// Stamped, but changed since.
    Edited,
}

/// `body` with the stamp line prepended.
pub fn stamp(body: &str) -> String {
    format!("{PREFIX}{}\n{body}", digest(body))
}

/// Whether `text` is still what the generator stamped.
pub fn provenance(text: &str) -> Provenance {
    let (first, body) = text.split_once('\n').unwrap_or((text, ""));
    match first.trim_end_matches('\r').strip_prefix(PREFIX) {
        None => Provenance::Unstamped,
        Some(hash) if hash == digest(body) => Provenance::Generated,
        Some(_) => Provenance::Edited,
    }
}

/// The lines of `current` that are not in `new`, as `(1-based line number, line)`, in file order:
/// what overwriting `current` with `new` discards. The stamp line is skipped — it changes on every
/// regeneration and was never anyone's edit.
pub fn discarded<'a>(current: &'a str, new: &str) -> Vec<(usize, &'a str)> {
    let old: Vec<&str> = current.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    // Longest-common-subsequence table, filled from the end so a forward walk can read it.
    // Workflows are a few hundred lines, so the quadratic table is small.
    let mut lcs = vec![vec![0u32; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lcs[i][j] = if same(old[i], new[j]) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < old.len() {
        if j < new.len() && same(old[i], new[j]) {
            i += 1;
            j += 1;
        } else if j < new.len() && lcs[i][j + 1] >= lcs[i + 1][j] {
            j += 1;
        } else {
            if !old[i].starts_with(PREFIX) {
                out.push((i + 1, old[i]));
            }
            i += 1;
        }
    }
    out
}

/// Lines compared as git with `autocrlf` would leave them: a CRLF checkout is not an edit.
fn same(a: &str, b: &str) -> bool {
    a.trim_end_matches('\r') == b.trim_end_matches('\r')
}

fn digest(body: &str) -> String {
    let normalized = body.replace("\r\n", "\n");
    Sha256::digest(normalized.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "name: Release\njobs:\n  build:\n    runs-on: ubuntu-latest\n";

    #[test]
    fn a_fresh_stamp_reads_back_as_generated() {
        let text = stamp(BODY);
        assert!(text.ends_with(BODY));
        assert_eq!(provenance(&text), Provenance::Generated);
        // A CRLF checkout of the same file is not a hand edit.
        assert_eq!(
            provenance(&text.replace('\n', "\r\n")),
            Provenance::Generated
        );
    }

    #[test]
    fn any_change_below_the_stamp_reads_as_edited() {
        let text = stamp(BODY).replace("ubuntu-latest", "ubuntu-22.04");
        assert_eq!(provenance(&text), Provenance::Edited);
        assert_eq!(provenance(BODY), Provenance::Unstamped);
    }

    /// The two regressions that motivated the stamp: a hand-added step and a hand-edited line. Both
    /// must be listed; the stamp line itself, which always differs, must not be.
    #[test]
    fn discarded_lists_the_hand_edits_and_nothing_else() {
        let generated = stamp(BODY);
        let edited = generated
            .replace(
                "  build:\n",
                "  build:\n    env:\n      ES_RUNTIME_INSPECTOR: \"1\"\n",
            )
            .replace("ubuntu-latest", "ubuntu-22.04");
        let regenerated = stamp(&format!("{BODY}# a newer generator line\n"));

        assert_eq!(
            discarded(&edited, &regenerated),
            vec![
                (5, "    env:"),
                (6, "      ES_RUNTIME_INSPECTOR: \"1\""),
                (7, "    runs-on: ubuntu-22.04"),
            ]
        );
        assert!(discarded(&generated, &regenerated).is_empty());
    }
}
