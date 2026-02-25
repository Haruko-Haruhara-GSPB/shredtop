//! `shred-probe discover` — show multicast memberships and configured sources.
//!
//! Queries the kernel for active multicast group memberships, lists configured
//! sources from probe.toml, and shows DoubleZero group metadata if the CLI is
//! installed. On completion, offers to write detected sources back to probe.toml.

use anyhow::Result;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

use crate::config::{ProbeConfig, SourceEntry};

pub fn run(config: &ProbeConfig, config_path: &Path) -> Result<()> {
    println!("=== DoubleZero available groups ===");
    let dz_groups = collect_and_show_dz_groups();

    println!();
    println!("=== Active multicast memberships ===");
    let memberships = collect_and_show_memberships();

    println!();
    println!("=== Active UDP sockets on multicast addresses ===");
    show_udp_sockets();

    println!();
    println!("=== Configured sources (probe.toml) ===");
    show_configured_sources(config);

    // Cross-reference doublezero groups with local memberships.
    let detected = detect_sources(&dz_groups, &memberships);

    if !detected.is_empty() {
        println!();
        println!("Detected {} active feed(s):", detected.len());
        for s in &detected {
            println!(
                "  {} — {} on {}",
                s.name,
                s.multicast_addr.as_deref().unwrap_or("?"),
                s.interface.as_deref().unwrap_or("?"),
            );
        }
        println!();
        print!(
            "Write detected sources to {}? [y/N] ",
            config_path.display()
        );
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if input.trim().eq_ignore_ascii_case("y") {
            let mut sources = detected;
            sources.push(SourceEntry {
                name: "rpc".into(),
                source_type: "rpc".into(),
                multicast_addr: None,
                port: None,
                interface: None,
                url: Some("http://127.0.0.1:8899".into()),
                pin_recv_core: None,
                pin_decode_core: None,
            });
            let cfg = ProbeConfig { sources };
            let toml_str = toml::to_string_pretty(&cfg)?;
            std::fs::write(config_path, toml_str)?;
            println!("Written to {}.", config_path.display());
            println!(
                "Edit the rpc url if your local node is not at http://127.0.0.1:8899"
            );
        }
    } else {
        println!();
        println!("Tip: to subscribe to a DoubleZero feed:");
        println!("  doublezero connect multicast --subscribe <code>");
        println!("  Then re-run `shred-probe discover`");
    }

    Ok(())
}

/// Query `doublezero multicast group list --json`, print the table, and return
/// a map of multicast_ip → code for all listed groups.
fn collect_and_show_dz_groups() -> HashMap<String, String> {
    let mut map = HashMap::new();

    if let Ok(output) = Command::new("doublezero")
        .args(["multicast", "group", "list", "--json"])
        .output()
    {
        if output.status.success() {
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
                        if ip != "?" {
                            map.insert(ip.to_string(), code.to_string());
                        }
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

    map
}

/// Parse `ip maddr show`, print active multicast memberships, and return a map
/// of multicast_ip → interface_name.
fn collect_and_show_memberships() -> HashMap<String, String> {
    #[cfg(target_os = "linux")]
    {
        let mut map = HashMap::new();
        if let Ok(output) = Command::new("ip").args(["maddr", "show"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut current_iface = String::new();
            for line in text.lines() {
                if line.starts_with(|c: char| c.is_ascii_digit()) {
                    if let Some(name) = line.split_whitespace().nth(1) {
                        current_iface = name.trim_end_matches(':').to_string();
                    }
                } else if line.trim().starts_with("inet ") {
                    let addr = line.trim().split_whitespace().nth(1).unwrap_or("");
                    let first_octet: u8 =
                        addr.split('.').next().unwrap_or("0").parse().unwrap_or(0);
                    if (224..=239).contains(&first_octet) {
                        println!("  {}  {}", current_iface, addr);
                        map.insert(addr.to_string(), current_iface.clone());
                    }
                }
            }
        } else {
            println!("  (ip command not available)");
        }
        map
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("  (multicast membership query requires Linux — ip maddr show)");
        HashMap::new()
    }
}

/// Cross-reference doublezero groups with local memberships to build a
/// SourceEntry list for feeds that are actively subscribed on this machine.
fn detect_sources(
    dz_groups: &HashMap<String, String>,
    memberships: &HashMap<String, String>,
) -> Vec<SourceEntry> {
    let mut sources: Vec<SourceEntry> = memberships
        .iter()
        .filter_map(|(ip, iface)| {
            dz_groups.get(ip).map(|code| SourceEntry {
                name: code.clone(),
                source_type: "shred".into(),
                multicast_addr: Some(ip.clone()),
                port: Some(20001),
                interface: Some(iface.clone()),
                url: None,
                pin_recv_core: None,
                pin_decode_core: None,
            })
        })
        .collect();

    sources.sort_by(|a, b| a.name.cmp(&b.name));
    sources
}

fn show_configured_sources(config: &ProbeConfig) {
    if config.sources.is_empty() {
        println!("  (none)");
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
}

/// Print UDP sockets bound to multicast addresses from `ss -ulnp`.
fn show_udp_sockets() {
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("ss").args(["-ulnp"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut found = false;
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() >= 5 {
                    let local = fields[4];
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
