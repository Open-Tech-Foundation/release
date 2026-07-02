use anyhow::{Context, Result};
use otf_release_core::git::parse_semver;
use std::process::Command;

/// Whether the running build is already at or ahead of `latest`, so no update should run.
///
/// Compares the parsed `x.y.z` cores rather than exact strings: a local dev build at `0.15.0`
/// must not "update" (i.e. downgrade) to a `0.14.0` latest release. Falls back to string equality
/// only when either version can't be parsed.
fn is_up_to_date(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some(current), Some(latest)) => latest <= current,
        _ => current == latest,
    }
}

pub fn run() -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");

    println!("Checking for updates...");

    // Fetch latest release from GitHub
    let url = "https://api.github.com/repos/Open-Tech-Foundation/release/releases/latest";
    let resp = ureq::get(url)
        .set("User-Agent", "otf-release-updater")
        .call()
        .context("failed to query GitHub API for latest release")?;

    let json: serde_json::Value = resp.into_json()?;
    let latest_tag = json["tag_name"]
        .as_str()
        .context("missing tag_name in github response")?;
    let latest_version = latest_tag.trim_start_matches('v');

    if is_up_to_date(current_version, latest_version) {
        println!(
            "You are already using the latest version (v{}).",
            current_version
        );
        return Ok(());
    }

    println!(
        "Updating otf-release from v{} to v{}...",
        current_version, latest_version
    );

    let (shell, arg, cmd) = if cfg!(windows) {
        (
            "powershell", 
            "-Command", 
            "irm https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.ps1 | iex"
        )
    } else {
        (
            "sh", 
            "-c", 
            "curl -fsSL https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.sh | bash"
        )
    };

    let status = Command::new(shell)
        .arg(arg)
        .arg(cmd)
        .status()
        .context("failed to execute installation script")?;

    if status.success() {
        println!(
            "Successfully updated otf-release from v{} to v{}!",
            current_version, latest_version
        );
    } else {
        anyhow::bail!("Installation script failed with status: {}", status);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_up_to_date;

    #[test]
    fn up_to_date_only_when_current_is_at_or_ahead_of_latest() {
        // An older build should update.
        assert!(!is_up_to_date("0.14.0", "0.15.0"));
        // Same version: no update.
        assert!(is_up_to_date("0.14.0", "0.14.0"));
        // A dev build ahead of the latest release must NOT downgrade.
        assert!(is_up_to_date("0.15.0", "0.14.0"));
        // Numeric, not lexical, comparison.
        assert!(!is_up_to_date("0.9.0", "0.10.0"));
    }
}
