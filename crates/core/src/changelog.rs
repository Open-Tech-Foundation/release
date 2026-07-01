//! Keep a Changelog parser/rewriter.
//!
//! The hand-written `[Unreleased]` section is the **source of truth** for release notes —
//! never inferred from commits. On apply, `[Unreleased]` is moved to a dated
//! `## [x.y.z] - YYYY-MM-DD` section and a fresh empty `[Unreleased]` is left behind.
//!
//! Section boundaries are level-2 ATX headings (`## …`); the `### Added`/`### Fixed`
//! subsections *inside* `[Unreleased]` (level 3) are part of the body, not boundaries.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// The parsed `[Unreleased]` section of a changelog.
#[derive(Debug, Clone, Default)]
pub struct Unreleased {
    /// Raw markdown body between the `## [Unreleased]` heading and the next `## ` heading.
    pub body: String,
}

impl Unreleased {
    /// True when the section has no meaningful content (whitespace / HTML comments only).
    pub fn is_empty(&self) -> bool {
        is_blank(&self.body)
    }
}

/// Parse the `[Unreleased]` section out of a changelog file. A missing section is treated as
/// empty (so the preflight "empty/missing" rule sees `is_empty() == true`).
pub fn parse_unreleased(changelog_path: &Path) -> Result<Unreleased> {
    let content = read(changelog_path)?;
    Ok(Unreleased {
        body: extract_unreleased_body(&content).unwrap_or_default(),
    })
}

/// Move `[Unreleased]` into a dated release section, leaving a fresh empty `[Unreleased]`.
/// Auto-bumped-only packages (`stub_if_empty == true` with no notes) receive the
/// `_Dependency updates._` stub.
pub fn release_unreleased(
    changelog_path: &Path,
    version: &str,
    date: &str,
    stub_if_empty: bool,
) -> Result<()> {
    let content = read(changelog_path)?;
    let rewritten = rewrite_release(&content, version, date, stub_if_empty)
        .with_context(|| format!("rewriting {}", changelog_path.display()))?;
    fs::write(changelog_path, rewritten)
        .with_context(|| format!("writing {}", changelog_path.display()))
}

/// Prepend generated git commits to the changelog as a new release section.
pub fn prepend_generated(
    changelog_path: &Path,
    version: &str,
    date: &str,
    generated_notes: &str,
) -> Result<()> {
    let content = if changelog_path.exists() {
        read(changelog_path)?
    } else {
        "# Changelog\n\n".to_string()
    };

    let notes = if generated_notes.trim().is_empty() {
        "\n_No changes._\n\n".to_string()
    } else {
        format!("\n{}\n\n", generated_notes.trim())
    };
    let release_section = format!("## [{version}] - {date}{notes}");

    let rewritten = if let Some((body_start, body_end)) = find_unreleased(&content) {
        let mut out = String::with_capacity(content.len() + 64);
        out.push_str(&content[..body_start]);
        out.push('\n');
        out.push_str(&release_section);
        out.push_str(&content[body_end..]);
        out
    } else if let Some(idx) = content.find("## ") {
        let mut out = content[..idx].to_string();
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(&release_section);
        out.push_str(&content[idx..]);
        out
    } else {
        let mut out = content.clone();
        if !out.ends_with("\n\n") {
            out.push_str("\n\n");
        }
        out.push_str(&release_section);
        out
    };

    fs::write(changelog_path, rewritten)
        .with_context(|| format!("writing {}", changelog_path.display()))
}

/// The trimmed body of a dated `## [version] - …` section, for a GitHub Release body.
/// Returns `None` if there is no such section.
pub fn dated_section_notes(changelog_path: &Path, version: &str) -> Result<Option<String>> {
    let content = read(changelog_path)?;
    Ok(section_notes(&content, version))
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

// ---- pure string logic (unit-tested directly) ---------------------------------------------

fn extract_unreleased_body(content: &str) -> Option<String> {
    let (start, end) = find_unreleased(content)?;
    Some(content[start..end].to_string())
}

fn rewrite_release(
    content: &str,
    version: &str,
    date: &str,
    stub_if_empty: bool,
) -> Result<String> {
    let Some((body_start, body_end)) = find_unreleased(content) else {
        if stub_if_empty {
            return Ok(rewrite_missing_unreleased_stub(content, version, date));
        }
        return Err(anyhow!("no `## [Unreleased]` section found"));
    };
    let body = &content[body_start..body_end];

    let body_out = if is_blank(body) {
        if stub_if_empty {
            "\n_Dependency updates._\n\n".to_string()
        } else {
            "\n".to_string()
        }
    } else {
        body.to_string()
    };

    let mut out = String::with_capacity(content.len() + 64);
    out.push_str(&content[..body_start]); // up to and including the `## [Unreleased]` line
    out.push('\n'); // blank line under the now-empty [Unreleased]
    out.push_str(&format!("## [{version}] - {date}\n"));
    out.push_str(&body_out);
    out.push_str(&content[body_end..]);
    Ok(out)
}

fn rewrite_missing_unreleased_stub(content: &str, version: &str, date: &str) -> String {
    let release = format!("## [{version}] - {date}\n\n_Dependency updates._\n\n");
    let mut insert = String::from("## [Unreleased]\n\n");
    insert.push_str(&release);

    if let Some(idx) = content.find("## ") {
        let mut out = content[..idx].to_string();
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(&insert);
        out.push_str(&content[idx..]);
        out
    } else {
        let mut out = content.to_string();
        if !out.ends_with("\n\n") {
            out.push_str("\n\n");
        }
        out.push_str(&insert);
        out
    }
}

fn section_notes(content: &str, version: &str) -> Option<String> {
    let needle = format!("[{version}]");
    let (start, end) = find_heading_section(content, |line| {
        heading_level(line) == Some(2) && heading_text(line).starts_with(&needle)
    })?;
    Some(content[start..end].trim().to_string())
}

/// Body span of the `[Unreleased]` section: from just after its heading line to the start of
/// the next level-≤2 heading (or EOF).
fn find_unreleased(content: &str) -> Option<(usize, usize)> {
    find_heading_section(content, is_unreleased_heading)
}

/// Generic: find the heading line matching `is_target`, then return the byte span of its body
/// up to the next level-≤2 heading.
fn find_heading_section(content: &str, is_target: impl Fn(&str) -> bool) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut body_start: Option<usize> = None;
    for line in content.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        match body_start {
            None => {
                if is_target(line) {
                    body_start = Some(offset);
                }
            }
            Some(_) => {
                if matches!(heading_level(line), Some(level) if level <= 2) {
                    return Some((body_start.unwrap(), line_start));
                }
            }
        }
    }
    body_start.map(|start| (start, content.len()))
}

fn is_unreleased_heading(line: &str) -> bool {
    heading_level(line) == Some(2)
        && heading_text(line)
            .to_ascii_lowercase()
            .starts_with("[unreleased]")
}

/// ATX heading level (count of leading `#`), or `None` if the line is not a heading.
fn heading_level(line: &str) -> Option<usize> {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &t[hashes..];
    // ATX requires the run of `#` to be followed by whitespace or end of line.
    if after.is_empty() || after.starts_with([' ', '\t', '\n', '\r']) {
        Some(hashes)
    } else {
        None
    }
}

/// The text of a heading line with its leading `#`s and surrounding whitespace stripped.
fn heading_text(line: &str) -> &str {
    line.trim_start().trim_start_matches('#').trim()
}

fn is_blank(s: &str) -> bool {
    strip_html_comments(s).trim().is_empty()
}

fn strip_html_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = ""; // unterminated comment: drop the remainder
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_NOTES: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n- thing\n\n## [1.0.0] - 2024-01-01\n\n- initial\n";
    const EMPTY_UNRELEASED: &str =
        "# Changelog\n\n## [Unreleased]\n\n## [1.0.0] - 2024-01-01\n\n- initial\n";

    #[test]
    fn extracts_unreleased_body_including_subsections() {
        let body = extract_unreleased_body(WITH_NOTES).unwrap();
        assert_eq!(body, "\n### Added\n- thing\n\n");
        assert!(!is_blank(&body));
    }

    #[test]
    fn missing_section_is_treated_as_empty() {
        let u = Unreleased {
            body: extract_unreleased_body("# Changelog\n\nno sections here\n").unwrap_or_default(),
        };
        assert!(u.is_empty());
    }

    #[test]
    fn whitespace_and_comment_only_unreleased_is_empty() {
        assert!(is_blank("\n  \n"));
        assert!(is_blank("\n<!-- nothing yet -->\n  \n"));
        assert!(!is_blank("\n- a real change\n"));
        let body = extract_unreleased_body(EMPTY_UNRELEASED).unwrap();
        assert!(is_blank(&body));
    }

    #[test]
    fn rewrite_moves_notes_under_dated_heading() {
        let out = rewrite_release(WITH_NOTES, "1.1.0", "2026-06-24", false).unwrap();
        assert_eq!(
            out,
            "# Changelog\n\n## [Unreleased]\n\n## [1.1.0] - 2026-06-24\n\n### Added\n- thing\n\n## [1.0.0] - 2024-01-01\n\n- initial\n"
        );
    }

    #[test]
    fn rewrite_stubs_empty_unreleased_when_requested() {
        let out = rewrite_release(EMPTY_UNRELEASED, "1.1.0", "2026-06-24", true).unwrap();
        assert_eq!(
            out,
            "# Changelog\n\n## [Unreleased]\n\n## [1.1.0] - 2026-06-24\n\n_Dependency updates._\n\n## [1.0.0] - 2024-01-01\n\n- initial\n"
        );
    }

    #[test]
    fn rewrite_stubs_missing_unreleased_when_auto_bumped() {
        let out = rewrite_release(
            "# Changelog\n\n## [1.0.0] - 2024-01-01\n\n- initial\n",
            "1.1.0",
            "2026-06-24",
            true,
        )
        .unwrap();
        assert_eq!(
            out,
            "# Changelog\n\n## [Unreleased]\n\n## [1.1.0] - 2026-06-24\n\n_Dependency updates._\n\n## [1.0.0] - 2024-01-01\n\n- initial\n"
        );
    }

    #[test]
    fn rewrite_errors_without_unreleased() {
        let err = rewrite_release(
            "# Changelog\n\n## [1.0.0] - 2024-01-01\n",
            "1.1.0",
            "2026-06-24",
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("[Unreleased]"));
    }

    #[test]
    fn dated_section_notes_are_extracted_and_trimmed() {
        let released = rewrite_release(WITH_NOTES, "1.1.0", "2026-06-24", false).unwrap();
        assert_eq!(
            section_notes(&released, "1.1.0").unwrap(),
            "### Added\n- thing"
        );
        assert_eq!(section_notes(&released, "9.9.9"), None);
    }

    #[test]
    fn heading_level_distinguishes_atx_levels() {
        assert_eq!(heading_level("## [Unreleased]\n"), Some(2));
        assert_eq!(heading_level("### Added\n"), Some(3));
        assert_eq!(heading_level("#hashtag\n"), None);
        assert_eq!(heading_level("not a heading\n"), None);
    }
}
