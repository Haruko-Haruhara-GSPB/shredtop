//! `shredder upgrade` — check for updates and reinstall from GitHub.

use anyhow::Result;
use std::io::{self, Write};

const REPO_URL: &str = "https://github.com/Haruko-Haruhara-GSPB/shred-probe.git";
const TAGS_API: &str =
    "https://api.github.com/repos/Haruko-Haruhara-GSPB/shred-probe/tags";

pub fn run() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current:  v{}", current);
    print!("Latest:   ");
    io::stdout().flush()?;

    let latest = fetch_latest_tag();
    match &latest {
        Some(tag) => println!("{}", tag),
        None => println!("(could not reach GitHub)"),
    }

    if let Some(ref tag) = latest {
        if tag == &format!("v{}", current) {
            println!("Already up to date.");
            return Ok(());
        }
    }

    println!();

    let tag_owned;
    let mut args = vec!["install", "--git", REPO_URL, "--force"];
    if let Some(ref tag) = latest {
        tag_owned = tag.clone();
        args.push("--tag");
        args.push(&tag_owned);
        println!("Upgrading to {}...", tag);
    } else {
        println!("Installing latest from main...");
    }

    let status = std::process::Command::new("cargo")
        .args(&args)
        .status()?;

    if !status.success() {
        anyhow::bail!("upgrade failed");
    }

    Ok(())
}

/// Query the GitHub tags API and return the name of the most recent tag.
/// Uses `curl` to avoid pulling in a TLS dependency.
fn fetch_latest_tag() -> Option<String> {
    let output = std::process::Command::new("curl")
        .args([
            "-sf",
            "--max-time",
            "10",
            "-H",
            "User-Agent: shredder",
            TAGS_API,
        ])
        .output()
        .ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    json.as_array()?
        .first()?
        .get("name")?
        .as_str()
        .map(str::to_string)
}
