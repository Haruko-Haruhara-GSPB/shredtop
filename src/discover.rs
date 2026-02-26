//! `shredder discover` — show multicast memberships and configured sources.
//!
//! Queries the kernel for active multicast group memberships, lists configured
//! sources from probe.toml, and shows DoubleZero group metadata if the CLI is
//! installed. On completion, offers to write detected sources back to probe.toml.

use anyhow::Result;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::config::{ProbeConfig, SourceEntry};

pub fn run(config: &ProbeConfig, config_path: &Path) -> Result<()> {
    println!("=== DoubleZero available groups ===");
    // Returns multicast_ip → (code, Option<port>). Port is Some if the DZ CLI
    // returns it in the JSON; None means we need to detect from traffic.
    let dz_groups = collect_and_show_dz_groups();

    println!();
    println!("=== Active multicast memberships ===");
    let memberships = collect_and_show_memberships();

    println!();
    println!("=== Configured sources (probe.toml) ===");
    show_configured_sources(config);

    // For subscribed groups not covered by CLI-provided port info, sniff
    // live traffic to detect which UDP port the shreds actually arrive on.
    let needs_sniff: Vec<(String, String)> = memberships
        .iter()
        .filter(|(ip, _)| {
            dz_groups
                .get(*ip)
                .and_then(|(_, p)| *p)
                .is_none()
        })
        .map(|(ip, iface): (&String, &String)| (ip.clone(), iface.clone()))
        .collect();

    let traffic_ports: HashMap<String, u16> = if !needs_sniff.is_empty() {
        println!();
        print!("Sniffing shred ports from live traffic (3s)...");
        io::stdout().flush().ok();
        let ports = detect_shred_ports_from_traffic(&needs_sniff);
        println!(" done.");
        if !ports.is_empty() {
            for (ip, port) in &ports {
                println!("  {} → port {}", ip, port);
            }
        }
        ports
    } else {
        HashMap::new()
    };

    // Show UDP sockets using all known/detected ports.
    let known_ports: Vec<u16> = dz_groups
        .values()
        .filter_map(|(_, p)| *p)
        .chain(traffic_ports.values().copied())
        .collect();
    println!();
    println!("=== Active UDP sockets on shred ports ===");
    show_udp_sockets(&known_ports);

    // Build detected source list.
    let detected = detect_sources(&dz_groups, &memberships, &traffic_ports);

    // -----------------------------------------------------------------------
    // Step 1: offer to include auto-detected feeds
    // -----------------------------------------------------------------------
    let mut sources_to_write: Vec<SourceEntry> = Vec::new();

    if !detected.is_empty() {
        println!();
        println!("Detected {} active feed(s):", detected.len());
        for s in &detected {
            println!(
                "  {} — {} port {} on {}",
                s.name,
                s.multicast_addr.as_deref().unwrap_or("?"),
                s.port.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
                s.interface.as_deref().unwrap_or("?"),
            );
        }
        // Warn about subscribed groups where port detection failed.
        for (ip, _iface) in &memberships {
            if let Some((code, None)) = dz_groups.get(ip) {
                if !traffic_ports.contains_key(ip) {
                    println!(
                        "  ⚠  {} — could not detect port (no traffic in 3s). \
                         Add it manually below.",
                        code
                    );
                }
            }
        }
        println!();
        if prompt_yn(&format!(
            "Include auto-detected sources in {}?",
            config_path.display()
        )) {
            // Auto-detect a baseline RPC if not already present.
            let rpc_url = detect_rpc_url();
            println!("  RPC baseline: {}", rpc_url);
            sources_to_write.extend(detected);
            sources_to_write.push(SourceEntry {
                name: "rpc".into(),
                source_type: "rpc".into(),
                multicast_addr: None,
                port: None,
                interface: None,
                url: Some(rpc_url),
                x_token: None,
                pin_recv_core: None,
                pin_decode_core: None,
            });
        }
    } else {
        println!();
        println!("No feeds auto-detected.");
        println!("  Tip: doublezero connect multicast --subscribe <code>");
    }

    // -----------------------------------------------------------------------
    // Step 2: let the user add any sources that weren't auto-detected
    // -----------------------------------------------------------------------
    println!();
    if prompt_yn("Are there any feed sources missing?") {
        let manual = collect_manual_sources();
        sources_to_write.extend(manual);
    }

    // -----------------------------------------------------------------------
    // Step 3: write probe.toml if we have anything
    // -----------------------------------------------------------------------
    if !sources_to_write.is_empty() {
        let cfg = ProbeConfig { sources: sources_to_write, filter_programs: Vec::new() };
        let toml_str = toml::to_string_pretty(&cfg)?;
        std::fs::write(config_path, toml_str)?;
        println!();
        println!("Written to {}.", config_path.display());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// DoubleZero group metadata
// ---------------------------------------------------------------------------

/// Query `doublezero multicast group list --json`, print the table, and return
/// a map of multicast_ip → (code, Option<port>).
///
/// The port is populated if the DoubleZero CLI JSON includes a port field;
/// otherwise it is None and will be filled in by traffic sniffing.
fn collect_and_show_dz_groups() -> HashMap<String, (String, Option<u16>)> {
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
                        // Try several candidate field names for the shred data port.
                        let port: Option<u16> = g["port"]
                            .as_u64()
                            .or_else(|| g["shred_port"].as_u64())
                            .or_else(|| g["data_port"].as_u64())
                            .and_then(|p| u16::try_from(p).ok());
                        println!(
                            "  {:<20} {:<16} {:>4} {:>4} {:<12} {}",
                            code, ip, pub_, sub, bw, status
                        );
                        if ip != "?" {
                            map.insert(ip.to_string(), (code.to_string(), port));
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

// ---------------------------------------------------------------------------
// Multicast memberships
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Traffic-based port detection
// ---------------------------------------------------------------------------

/// Sniff live UDP traffic to determine which port each subscribed multicast
/// group is using for shred data.
///
/// Runs a brief `tcpdump` capture (up to 3 seconds) on each interface and
/// parses the destination port from the first packet seen for each group.
/// Requires `tcpdump` to be installed and sufficient privileges (root).
fn detect_shred_ports_from_traffic(
    groups: &[(String, String)], // (multicast_ip, interface)
) -> HashMap<String, u16> {
    // Group by interface so we run one tcpdump per interface.
    let mut by_iface: HashMap<String, Vec<String>> = HashMap::new();
    for (ip, iface) in groups {
        by_iface.entry(iface.clone()).or_default().push(ip.clone());
    }

    let mut result: HashMap<String, u16> = HashMap::new();

    for (iface, ips) in &by_iface {
        // Build a pcap filter matching only packets destined for these IPs.
        let filter = ips
            .iter()
            .map(|ip| format!("dst {}", ip))
            .collect::<Vec<_>>()
            .join(" or ");

        // Use `timeout` to enforce a wall-clock limit; `-c 30` caps packet count.
        let output = Command::new("timeout")
            .args(["3", "tcpdump", "-c", "30", "-ni", iface, "-q", &filter])
            .output();

        let Ok(output) = output else { continue };

        // tcpdump writes packet lines to stdout; combine with stderr for safety.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stdout.lines().chain(stderr.lines()) {
            if let Some((ip, port)) = parse_dst_from_tcpdump_line(line) {
                if ips.contains(&ip) {
                    result.entry(ip).or_insert(port);
                }
            }
            // Stop early if we have ports for all groups on this interface.
            if ips.iter().all(|ip| result.contains_key(ip)) {
                break;
            }
        }
    }

    result
}

/// Parse a tcpdump `-q` output line and return the destination (multicast IP, port).
///
/// Expected format: `HH:MM:SS.usec IP src.sport > dst.dport: UDP, length N`
/// The dst token is `A.B.C.D.PORT:` — we rsplit on `.` to separate IP from port.
fn parse_dst_from_tcpdump_line(line: &str) -> Option<(String, u16)> {
    let gt = line.find(" > ")?;
    let after = &line[gt + 3..];
    let token = after.split_whitespace().next()?.trim_end_matches(':');
    let dot = token.rfind('.')?;
    let ip = &token[..dot];
    let port: u16 = token[dot + 1..].parse().ok()?;
    // Sanity-check: destination should be a multicast address (224–239).
    let first_octet: u8 = ip.split('.').next()?.parse().ok()?;
    if !(224..=239).contains(&first_octet) {
        return None;
    }
    Some((ip.to_string(), port))
}

// ---------------------------------------------------------------------------
// Source building
// ---------------------------------------------------------------------------

/// Cross-reference doublezero groups with local memberships to build a
/// SourceEntry list for feeds that are actively subscribed on this machine.
///
/// Port priority: DoubleZero CLI JSON → traffic sniff → None (excluded with warning).
fn detect_sources(
    dz_groups: &HashMap<String, (String, Option<u16>)>,
    memberships: &HashMap<String, String>,
    traffic_ports: &HashMap<String, u16>,
) -> Vec<SourceEntry> {
    let mut sources: Vec<SourceEntry> = memberships
        .iter()
        .filter_map(|(ip, iface)| {
            let (code, dz_port) = dz_groups.get(ip)?;
            let port = dz_port
                .or_else(|| traffic_ports.get(ip).copied());
            // Exclude sources where port is unknown — would silently use wrong default.
            let port = port?;
            Some(SourceEntry {
                name: code.clone(),
                source_type: "shred".into(),
                multicast_addr: Some(ip.clone()),
                port: Some(port),
                interface: Some(iface.clone()),
                url: None,
                x_token: None,
                pin_recv_core: None,
                pin_decode_core: None,
            })
        })
        .collect();

    sources.sort_by(|a, b| a.name.cmp(&b.name));
    sources
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

fn show_configured_sources(config: &ProbeConfig) {
    if config.sources.is_empty() {
        println!("  (none)");
    } else {
        println!(
            "  {:<20} {:<10} {:<20} {:<8} {:<14}",
            "NAME", "TYPE", "MULTICAST/URL", "PORT", "INTERFACE"
        );
        println!("  {}", "-".repeat(72));
        for s in &config.sources {
            match s.source_type.as_str() {
                "shred" => {
                    println!(
                        "  {:<20} {:<10} {:<20} {:<8} {:<14}",
                        s.name,
                        "shred",
                        s.multicast_addr.as_deref().unwrap_or("(default)"),
                        s.port.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
                        s.interface.as_deref().unwrap_or("doublezero1"),
                    );
                }
                "rpc" | "geyser" | "jito-grpc" => {
                    println!(
                        "  {:<20} {:<10} {:<20} {:<8} {:<14}",
                        s.name,
                        s.source_type,
                        s.url.as_deref().unwrap_or("-"),
                        "-",
                        "-",
                    );
                }
                other => {
                    println!("  {:<20} {:<10} (unknown type: {})", s.name, other, other);
                }
            }
        }
    }
}

/// Print UDP sockets listening on known shred ports from `ss -ulnp`.
///
/// Multicast receivers bind to 0.0.0.0:<port> (not the multicast IP), so we
/// look for sockets on the detected/known ports rather than filtering by IP.
fn show_udp_sockets(ports: &[u16]) {
    #[cfg(target_os = "linux")]
    {
        let port_strs: Vec<String> = if ports.is_empty() {
            // Fall back to well-known DoubleZero ports if none detected yet.
            vec!["7733".into(), "20001".into()]
        } else {
            // Deduplicate.
            let mut seen = std::collections::HashSet::new();
            ports
                .iter()
                .filter(|p| seen.insert(*p))
                .map(|p| p.to_string())
                .collect()
        };

        if let Ok(output) = Command::new("ss").args(["-ulnp"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut found = false;
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() >= 5 {
                    let local = fields[4];
                    let port = local.rsplit(':').next().unwrap_or("");
                    if port_strs.iter().any(|p| p == port) {
                        let process = fields.get(6).copied().unwrap_or("");
                        println!("  UDP {}  {}", local, process);
                        found = true;
                    }
                }
            }
            if !found {
                println!(
                    "  (no shred receivers found on port(s) {})",
                    port_strs.join(", ")
                );
            }
        } else {
            println!("  (ss command not available)");
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = ports;
        println!("  (UDP socket query requires Linux — ss -ulnp)");
    }
}

// ---------------------------------------------------------------------------
// RPC detection
// ---------------------------------------------------------------------------

/// Probe candidate localhost RPC ports and return the URL of the first one
/// that responds to a Solana `getHealth` JSON-RPC call.
fn detect_rpc_url() -> String {
    const CANDIDATES: &[u16] = &[8899, 58000, 8900, 9000, 8080];
    const BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"getHealth"}"#;

    for &port in CANDIDATES {
        let addr = format!("127.0.0.1:{}", port);
        let Ok(mut stream) =
            TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(300))
        else {
            continue;
        };
        let req = format!(
            "POST / HTTP/1.0\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            port,
            BODY.len(),
            BODY
        );
        if stream.write_all(req.as_bytes()).is_err() {
            continue;
        }
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        if response.contains("\"result\"") {
            return format!("http://127.0.0.1:{}", port);
        }
    }

    "http://127.0.0.1:8899".to_string()
}

// ---------------------------------------------------------------------------
// Interactive source builder
// ---------------------------------------------------------------------------

/// Ask the user to describe one or more sources that weren't auto-detected.
fn collect_manual_sources() -> Vec<SourceEntry> {
    let mut sources = Vec::new();
    loop {
        println!();
        println!("Source type:");
        println!("  1) shred     — DoubleZero / Jito ShredStream UDP multicast");
        println!("  2) rpc       — Solana JSON-RPC (local or remote)");
        println!("  3) geyser    — Yellowstone gRPC (Helius, Triton, QuickNode, …)");
        println!("  4) jito-grpc — Jito ShredStream gRPC proxy (local)");
        print!("Type [1-4]: ");
        io::stdout().flush().ok();

        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        let choice = input.trim().to_string();

        let entry = match choice.as_str() {
            "1" | "shred" => {
                let name = prompt_required("  Name", "e.g. bebop");
                let multicast_addr = prompt_required("  Multicast IP", "e.g. 233.84.178.1");
                let port_str = prompt_with_default("  Port", "20001", "e.g. 7733 for bebop, 20001 for jito");
                let port: u16 = match port_str.parse() {
                    Ok(p) => p,
                    Err(_) => { println!("  Invalid port — skipping source."); continue; }
                };
                let interface = prompt_with_default("  Interface", "doublezero1", "network interface");
                SourceEntry {
                    name,
                    source_type: "shred".into(),
                    multicast_addr: Some(multicast_addr),
                    port: Some(port),
                    interface: Some(interface),
                    url: None,
                    x_token: None,
                    pin_recv_core: None,
                    pin_decode_core: None,
                }
            }
            "2" | "rpc" => {
                let name = prompt_with_default("  Name", "rpc", "display name");
                let url = prompt_required("  URL", "e.g. http://127.0.0.1:8899");
                SourceEntry {
                    name,
                    source_type: "rpc".into(),
                    multicast_addr: None,
                    port: None,
                    interface: None,
                    url: Some(url),
                    x_token: None,
                    pin_recv_core: None,
                    pin_decode_core: None,
                }
            }
            "3" | "geyser" => {
                let name = prompt_required("  Name", "e.g. helius");
                let url = prompt_required("  URL", "e.g. https://mainnet.helius-rpc.com:443");
                let x_token = prompt_optional("  x-token", "auth token — press Enter to skip");
                SourceEntry {
                    name,
                    source_type: "geyser".into(),
                    multicast_addr: None,
                    port: None,
                    interface: None,
                    url: Some(url),
                    x_token,
                    pin_recv_core: None,
                    pin_decode_core: None,
                }
            }
            "4" | "jito-grpc" => {
                let name = prompt_with_default("  Name", "jito-grpc", "display name");
                let url = prompt_with_default("  URL", "http://127.0.0.1:9999", "proxy address");
                SourceEntry {
                    name,
                    source_type: "jito-grpc".into(),
                    multicast_addr: None,
                    port: None,
                    interface: None,
                    url: Some(url),
                    x_token: None,
                    pin_recv_core: None,
                    pin_decode_core: None,
                }
            }
            _ => {
                println!("  Unknown type — enter 1, 2, 3, or 4.");
                continue;
            }
        };

        println!("  Added: {}", entry.name);
        sources.push(entry);

        println!();
        if !prompt_yn("Add another source?") {
            break;
        }
    }
    sources
}

// ---------------------------------------------------------------------------
// Prompt helpers
// ---------------------------------------------------------------------------

fn prompt_yn(question: &str) -> bool {
    print!("{} [y/N] ", question);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    input.trim().eq_ignore_ascii_case("y")
}

/// Prompt for a required field — re-asks until the user enters something.
fn prompt_required(label: &str, hint: &str) -> String {
    loop {
        print!("{} ({}): ", label, hint);
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        let val = input.trim().to_string();
        if !val.is_empty() {
            return val;
        }
        println!("  Required — please enter a value.");
    }
}

/// Prompt for a field with a default value shown in brackets.
fn prompt_with_default(label: &str, default: &str, hint: &str) -> String {
    print!("{} [{}] ({}): ", label, default, hint);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let val = input.trim().to_string();
    if val.is_empty() { default.to_string() } else { val }
}

/// Prompt for an optional field — returns None if the user presses Enter.
fn prompt_optional(label: &str, hint: &str) -> Option<String> {
    print!("{} ({}): ", label, hint);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let val = input.trim().to_string();
    if val.is_empty() { None } else { Some(val) }
}
