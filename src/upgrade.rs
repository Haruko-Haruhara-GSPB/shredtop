//! `shredder upgrade` — download the latest release binary from GitHub.

use anyhow::Result;
use std::io::{self, Write};
use std::process::Command;

const REPO_DIR: &str = "~/shred-probe";

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

/// Pull main and rebuild from source. Skips the release pipeline entirely.
/// Useful during active development when releases haven't caught up.
pub fn run_from_source() -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let repo = std::path::PathBuf::from(&home).join("shred-probe");
    let repo_str = repo.to_str().unwrap();

    if repo.exists() {
        println!("Pulling latest main...");
        let ok = Command::new("git")
            .args(["-C", repo_str, "pull", "origin", "main"])
            .status()?
            .success();
        anyhow::ensure!(ok, "git pull failed");
    } else {
        println!("Cloning to {}...", repo_str);
        let ok = Command::new("git")
            .args(["clone", "https://github.com/Haruko-Haruhara-GSPB/shred-probe.git", repo_str])
            .status()?
            .success();
        anyhow::ensure!(ok, "git clone failed");
    }

    println!("Building...");
    let ok = Command::new("cargo")
        .args(["build", "--release", "--quiet"])
        .current_dir(&repo)
        .status()?
        .success();
    anyhow::ensure!(ok, "cargo build failed");

    // Copy the built binary over the currently installed one
    let built = repo.join("target/release/shredder");
    let dest = which_shredder()?;
    std::fs::copy(&built, &dest)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)?;
    }

    println!("Done. Built from main, installed to {}.", dest.display());
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
