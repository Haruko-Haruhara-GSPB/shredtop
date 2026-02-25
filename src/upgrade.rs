//! `shredder upgrade` — check for updates and reinstall.
//!
//! Fast path: if run from inside the cloned repo directory, uses
//! `git pull` + `cargo build --release` + binary copy. This reuses the
//! incremental build cache so only changed crates are recompiled.
//!
//! Slow path: if run from outside the repo, falls back to
//! `cargo install --git` which always does a full recompile.

use anyhow::Result;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

const REPO_URL: &str = "https://github.com/Haruko-Haruhara-GSPB/shred-probe.git";
const RELEASES_API: &str = "https://api.github.com/repos/Haruko-Haruhara-GSPB/shred-probe/releases/latest";

pub fn run() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current:  v{}", current);
    print!("Latest:   ");
    io::stdout().flush()?;

    let latest = fetch_latest_release();
    match &latest {
        Some(tag) => println!("{}", tag),
        None => println!("(could not reach GitHub)"),
    }

    if let Some(ref tag) = latest {
        if tag == &format!("v{}", current) {
            println!("Already up to date.");
            return Ok(());
        }
        println!("Upgrading to {}...", tag);
    } else {
        println!("Installing latest...");
    }

    println!();

    if is_repo_root() {
        fast_upgrade()
    } else {
        println!("Tip: run `shredder upgrade` from inside the cloned repo for faster upgrades.");
        println!("     cd shred-probe && shredder upgrade");
        println!();
        slow_upgrade(&latest)
    }
}

/// Fast path — git pull + incremental build + copy binary.
fn fast_upgrade() -> Result<()> {
    println!("Repo detected — using incremental build (fast).");
    println!();

    let ok = Command::new("git").args(["pull"]).status()?.success();
    anyhow::ensure!(ok, "git pull failed");

    let ok = Command::new("cargo")
        .args(["build", "--release", "--quiet"])
        .status()?
        .success();
    anyhow::ensure!(ok, "cargo build failed");

    // Copy the freshly built binary over the installed one.
    let built = Path::new("target/release/shredder");
    anyhow::ensure!(built.exists(), "build succeeded but binary not found");

    let dest = which_shredder()?;
    std::fs::copy(built, &dest)?;
    println!("Installed to {}.", dest.display());

    Ok(())
}

/// Slow path — cargo install --git (full recompile, no local clone needed).
fn slow_upgrade(latest: &Option<String>) -> Result<()> {
    let tag_owned;
    let mut args = vec!["install", "--git", REPO_URL, "--force", "--quiet"];
    if let Some(ref tag) = latest {
        tag_owned = tag.clone();
        args.push("--tag");
        args.push(&tag_owned);
    }

    let ok = Command::new("cargo").args(&args).status()?.success();
    anyhow::ensure!(ok, "upgrade failed");

    Ok(())
}

/// Returns true if the current directory looks like the shredder repo root.
fn is_repo_root() -> bool {
    Path::new("Cargo.toml").exists()
        && Path::new("crates/shred-ingest").exists()
}

/// Locate the installed shredder binary via `which`.
fn which_shredder() -> Result<std::path::PathBuf> {
    let out = Command::new("which").arg("shredder").output()?;
    let path = std::str::from_utf8(&out.stdout)?.trim().to_string();
    anyhow::ensure!(!path.is_empty(), "could not locate installed shredder binary");
    Ok(std::path::PathBuf::from(path))
}

/// Query the GitHub releases API and return the tag name of the latest release.
/// Uses `curl` to avoid pulling in a TLS dependency.
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
