//! `shred-probe discover` — show multicast memberships and configured sources.
//!
//! Queries the kernel for active multicast group memberships, lists configured
//! sources from probe.toml, and shows DoubleZero group metadata if the CLI is
//! installed.

use anyhow::Result;
use std::process::Command;

use crate::config::ProbeConfig;

pub fn run(config: &ProbeConfig) -> Result<()> {
    println!("=== DoubleZero available groups ===");
    if let Ok(output) = Command::new("doublezero")
        .args(["multicast", "group", "list", "--json"])
        .output()
    {
        if output.status.success() {
            // Pretty-print the JSON group list
            let json = String::from_utf8_lossy(&output.stdout);
            if let Ok(groups) = serde_json::from_str::<serde_json::Value>(&json) {
                if let Some(arr) = groups.as_array() {
                    println!(
                        "  {:<20} {:<16} {:>4} {:>4} {:<12} {}",
                        "CODE", "MULTICAST IP", "PUB", "SUB", "BANDWIDTH", "STATUS"
                    );
                    println!("  {}", "-".repeat(72));
                    for g in arr {
                        let code = g["code"].as_str().unwrap_or("?");
                        let ip = g["multicast_ip"].as_str().unwrap_or("?");
                        let pub_ = g["publishers"].as_u64().unwrap_or(0);
                        let sub = g["subscribers"].as_u64().unwrap_or(0);
                        let bw = g["max_bandwidth"].as_str().unwrap_or("?");
                        let status = g["status"].as_str().unwrap_or("?");
                        println!(
                            "  {:<20} {:<16} {:>4} {:>4} {:<12} {}",
                            code, ip, pub_, sub, bw, status
                        );
                    }
                } else {
                    println!("{}", json.trim());
                }
            } else {
                println!("{}", String::from_utf8_lossy(&output.stdout).trim());
            }
        } else {
            println!("  doublezero CLI returned error");
        }
    } else {
        println!("  doublezero CLI not found — install from https://doublezero.xyz");
    }

    println!();
    println!("=== Active multicast memberships ===");
    show_multicast_memberships();

    println!();
    println!("=== Active UDP sockets on multicast addresses ===");
    show_udp_sockets();

    println!();
    println!("=== Configured sources (probe.toml) ===");
    if config.sources.is_empty() {
        println!("  (no sources configured — run `shred-probe init` to create probe.toml)");
    } else {
        println!(
            "  {:<20} {:<6} {:<20} {:<8} {:<14}",
            "NAME", "TYPE", "MULTICAST/URL", "PORT", "INTERFACE"
        );
        println!("  {}", "-".repeat(72));
        for s in &config.sources {
            match s.source_type.as_str() {
                "shred" => {
                    println!(
                        "  {:<20} {:<6} {:<20} {:<8} {:<14}",
                        s.name,
                        "shred",
                        s.multicast_addr.as_deref().unwrap_or("(default)"),
                        s.port.map(|p| p.to_string()).unwrap_or_else(|| "20001".into()),
                        s.interface.as_deref().unwrap_or("doublezero1"),
                    );
                }
                "rpc" => {
                    println!(
                        "  {:<20} {:<6} {:<20} {:<8} {:<14}",
                        s.name,
                        "rpc",
                        s.url.as_deref().unwrap_or("http://127.0.0.1:8899"),
                        "-",
                        "-",
                    );
                }
                other => {
                    println!("  {:<20} {:<6} (unknown type: {})", s.name, other, other);
                }
            }
        }
    }

    println!();
    println!("Tip: to subscribe to a new DoubleZero feed:");
    println!("  doublezero connect multicast --subscribe <code>");
    println!("  Then add a [[sources]] block to probe.toml");

    Ok(())
}

/// Print active IPv4 multicast memberships from `ip maddr show`.
fn show_multicast_memberships() {
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("ip").args(["maddr", "show"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut current_iface = String::new();
            for line in text.lines() {
                if line.starts_with(|c: char| c.is_ascii_digit()) {
                    // Interface line: "2: doublezero1  ..."
                    if let Some(name) = line.split_whitespace().nth(1) {
                        current_iface = name.trim_end_matches(':').to_string();
                    }
                } else if line.trim().starts_with("inet ") {
                    let addr = line.trim().split_whitespace().nth(1).unwrap_or("");
                    // Only show multicast range (224.x.x.x – 239.x.x.x)
                    if addr.starts_with("2") {
                        let first_octet: u8 =
                            addr.split('.').next().unwrap_or("0").parse().unwrap_or(0);
                        if (224..=239).contains(&first_octet) {
                            println!("  {}  {}", current_iface, addr);
                        }
                    }
                }
            }
        } else {
            println!("  (ip command not available)");
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("  (multicast membership query requires Linux — ip maddr show)");
    }
}

/// Print UDP sockets bound to multicast addresses from `ss -ulnp`.
fn show_udp_sockets() {
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("ss").args(["-ulnp"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut found = false;
            for line in text.lines().skip(1) {
                // Local address column is field 4 (0-indexed)
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() >= 5 {
                    let local = fields[4];
                    // Strip port suffix: "233.84.178.1:20001" → first octet
                    let ip = local.split(':').next().unwrap_or("");
                    let first_octet: u8 =
                        ip.split('.').next().unwrap_or("0").parse().unwrap_or(0);
                    if (224..=239).contains(&first_octet) {
                        let process = fields.get(6).copied().unwrap_or("");
                        println!("  UDP {}  {}", local, process);
                        found = true;
                    }
                }
            }
            if !found {
                println!("  (no multicast UDP sockets found)");
            }
        } else {
            println!("  (ss command not available)");
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("  (UDP socket query requires Linux — ss -ulnp)");
    }
}
