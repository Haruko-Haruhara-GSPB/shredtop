//! `shredder upgrade` — download the latest release binary from GitHub.

use anyhow::Result;
use std::io::{self, Write};
use std::process::Command;

const RELEASES_API: &str =
    "https://api.github.com/repos/Haruko-Haruhara-GSPB/shred-probe/releases/latest";
const DOWNLOAD_URL: &str =
    "https://github.com/Haruko-Haruhara-GSPB/shred-probe/releases/download/{tag}/shredder";

pub fn run() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current:  v{}", current);
    print!("Latest:   ");
    io::stdout().flush()?;

    let latest = fetch_latest_release();
    match &latest {
        Some(tag) => println!("{}", tag),
        None => {
            println!("(could not reach GitHub)");
            return Ok(());
        }
    }

    let tag = latest.unwrap();
    if tag == format!("v{}", current) {
        println!("Already up to date.");
        return Ok(());
    }

    println!("Upgrading to {}...", tag);

    let url = DOWNLOAD_URL.replace("{tag}", &tag);
    let dest = which_shredder()?;

    let ok = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", "-o"])
        .arg(&dest)
        .arg(&url)
        .status()?
        .success();
    anyhow::ensure!(ok, "download failed — check your internet connection");

    // Ensure the binary is executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)?;
    }

    println!("Done. {} installed to {}.", tag, dest.display());
    Ok(())
}

/// Locate the installed shredder binary via `which`.
fn which_shredder() -> Result<std::path::PathBuf> {
    let out = Command::new("which").arg("shredder").output()?;
    let path = std::str::from_utf8(&out.stdout)?.trim().to_string();
    anyhow::ensure!(!path.is_empty(), "could not locate installed shredder binary");
    Ok(std::path::PathBuf::from(path))
}

/// Query the GitHub releases API and return the tag name of the latest release.
fn fetch_latest_release() -> Option<String> {
    let output = Command::new("curl")
        .args(["-sf", "--max-time", "10", "-H", "User-Agent: shredder", RELEASES_API])
        .output()
        .ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    json.get("tag_name")?.as_str().map(str::to_string)
}
