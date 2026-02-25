//! `shredder service` — systemd integration.
//!
//! Installs and manages a systemd unit that runs `shredder run` in the
//! background, logging metrics to /var/log/shredder.jsonl.

use anyhow::Result;
use std::process::Command;

const UNIT_PATH: &str = "/etc/systemd/system/shredder.service";

pub fn install(config_path: &std::path::Path) -> Result<()> {
    let binary = std::env::current_exe()?;
    let config_abs = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());

    let unit = format!(
        r#"[Unit]
Description=Shredder — Solana shred feed latency monitor
After=network.target

[Service]
Type=simple
User=root
ExecStart={binary} run -c {config}
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
"#,
        binary = binary.display(),
        config = config_abs.display(),
    );

    std::fs::write(UNIT_PATH, unit)?;

    let _ = Command::new("systemctl").arg("daemon-reload").status();
    let _ = Command::new("systemctl").args(["enable", "shredder"]).status();
    let _ = Command::new("systemctl").args(["start", "shredder"]).status();

    println!("Service installed, enabled, and started.");
    println!();
    println!("  shredder monitor  — open live dashboard");
    println!("  shredder status   — view latest metrics");

    Ok(())
}

pub fn uninstall() -> Result<()> {
    let _ = Command::new("systemctl").args(["stop", "shredder"]).status();
    let _ = Command::new("systemctl")
        .args(["disable", "shredder"])
        .status();
    std::fs::remove_file(UNIT_PATH)?;
    let _ = Command::new("systemctl").arg("daemon-reload").status();
    println!("Removed {}.", UNIT_PATH);
    Ok(())
}

pub fn control(action: &str) -> Result<()> {
    let ok = Command::new("systemctl")
        .args([action, "shredder"])
        .status()?
        .success();
    anyhow::ensure!(ok, "systemctl {} shredder failed", action);
    Ok(())
}
