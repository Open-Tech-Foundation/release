use anyhow::{Context, Result};
use std::process::Command;

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

    if current_version == latest_version {
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
