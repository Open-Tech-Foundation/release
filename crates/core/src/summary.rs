//! Confirmation / dry-run rendering shown before any files are written.
//!
//! Renders the three blocks from `docs/commands/version.md`: packages to publish
//! (selected), auto-bumped dependents, and internal range updates (including private
//! apps, which are flagged "range updated, NOT published").

/// A version change for one publishable package.
#[derive(Debug, Clone)]
pub struct VersionChange {
    pub name: String,
    pub old_version: String,
    pub new_version: String,
    /// True when the user explicitly selected this package; false when reached via cascade.
    pub selected: bool,
    /// The parenthetical reason, e.g. `major, selected` or `patch — depends on @x/core`.
    pub note: String,
}

/// An internal dependency-range update on one consumer.
#[derive(Debug, Clone)]
pub struct RangeUpdate {
    pub consumer: String,
    pub dep: String,
    pub old_range: String,
    pub new_range: String,
    /// True for private apps: the range is updated but the package is never published.
    pub consumer_private: bool,
}

/// The full plan for a `version` run.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub changes: Vec<VersionChange>,
    pub range_updates: Vec<RangeUpdate>,
}

/// Render the human-readable confirmation summary (no trailing prompt).
pub fn render(plan: &Plan) -> String {
    let mut out = String::new();

    let selected: Vec<&VersionChange> = plan.changes.iter().filter(|c| c.selected).collect();
    let auto: Vec<&VersionChange> = plan.changes.iter().filter(|c| !c.selected).collect();

    if !selected.is_empty() {
        out.push_str("Packages to publish:\n");
        for c in &selected {
            out.push_str(&format!(
                "  {}  {} → {}  ({})\n",
                c.name, c.old_version, c.new_version, c.note
            ));
        }
        out.push('\n');
    }

    if !auto.is_empty() {
        out.push_str("Auto-bumped dependents:\n");
        for c in &auto {
            out.push_str(&format!(
                "  {}  {} → {}  ({})\n",
                c.name, c.old_version, c.new_version, c.note
            ));
        }
        out.push('\n');
    }

    if !plan.range_updates.is_empty() {
        out.push_str("Internal range updates:\n");
        for r in &plan.range_updates {
            let private = if r.consumer_private {
                "   (private — range updated, NOT published)"
            } else {
                ""
            };
            out.push_str(&format!(
                "  {}:  {} {} → {}{}\n",
                r.consumer, r.dep, r.old_range, r.new_range, private
            ));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_all_three_blocks() {
        let plan = Plan {
            changes: vec![
                VersionChange {
                    name: "@opentf/core".into(),
                    old_version: "1.2.0".into(),
                    new_version: "2.0.0".into(),
                    selected: true,
                    note: "major, selected".into(),
                },
                VersionChange {
                    name: "@opentf/sdk".into(),
                    old_version: "1.0.0".into(),
                    new_version: "2.0.0".into(),
                    selected: false,
                    note: "mirror major — peerDep on @opentf/core".into(),
                },
            ],
            range_updates: vec![RangeUpdate {
                consumer: "playground".into(),
                dep: "@opentf/core".into(),
                old_range: "^1.2.0".into(),
                new_range: "^2.0.0".into(),
                consumer_private: true,
            }],
        };

        let out = render(&plan);
        assert!(out.contains("Packages to publish:"));
        assert!(out.contains("@opentf/core  1.2.0 → 2.0.0  (major, selected)"));
        assert!(out.contains("Auto-bumped dependents:"));
        assert!(out.contains("mirror major — peerDep on @opentf/core"));
        assert!(out.contains("Internal range updates:"));
        assert!(out.contains("(private — range updated, NOT published)"));
    }

    #[test]
    fn omits_empty_blocks() {
        let plan = Plan {
            changes: vec![VersionChange {
                name: "a".into(),
                old_version: "1.0.0".into(),
                new_version: "1.0.1".into(),
                selected: true,
                note: "patch, selected".into(),
            }],
            range_updates: vec![],
        };
        let out = render(&plan);
        assert!(out.contains("Packages to publish:"));
        assert!(!out.contains("Auto-bumped dependents:"));
        assert!(!out.contains("Internal range updates:"));
    }
}
