use anyhow::{Context, Result, bail};
use mouse_thing::core::policies::DISCOVERY_SERVICE_TYPE;
use mouse_thing::discovery::{DiscoveryClient, DiscoveryEvent};
use std::collections::HashMap;
use std::env;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

const DEFAULT_PORT: u16 = mouse_thing::core::policies::DEFAULT_PORT;
const DEFAULT_SNAPSHOT_INTERVAL_SECS: u64 = 5;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const DISCOVERY_LOG_TARGET: &str = "mouse_thing::core::services::discovery";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    port: u16,
    name: Option<String>,
    snapshot_interval: Duration,
    run_for: Option<Duration>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            name: None,
            snapshot_interval: Duration::from_secs(DEFAULT_SNAPSHOT_INTERVAL_SECS),
            run_for: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedArgs {
    Run(Args),
    Help(String),
}

#[derive(Debug, Clone)]
struct SeenPeer {
    primary_addr: std::net::SocketAddr,
    addrs: Vec<std::net::SocketAddr>,
    hostname: String,
    first_seen: Instant,
    last_seen: Instant,
    discoveries: u64,
}

fn main() -> Result<()> {
    let program_name = env::args()
        .next()
        .unwrap_or_else(|| "discovery-smoke".to_string());
    let args = match parse_args_from(env::args().skip(1), &program_name)? {
        ParsedArgs::Run(args) => args,
        ParsedArgs::Help(help) => {
            println!("{help}");
            return Ok(());
        }
    };

    mouse_thing::logging::init_runtime_discovery_logging();

    let local_hostname = local_hostname();
    let local_name = args.name.clone().unwrap_or_else(|| local_hostname.clone());
    let local_peer_id = local_peer_id();

    let running = Arc::new(AtomicBool::new(true));
    let running_for_handler = Arc::clone(&running);
    ctrlc::set_handler(move || {
        running_for_handler.store(false, Ordering::SeqCst);
    })
    .context("failed to install Ctrl+C handler")?;

    let mut discovery = DiscoveryClient::new();
    let mut rx = discovery
        .start(
            args.port,
            Some(local_name.as_str()),
            Some(local_peer_id.as_str()),
            None,
        )
        .with_context(|| format!("failed to start discovery on UDP port {}", args.port))?;

    info!(
        target: DISCOVERY_LOG_TARGET,
        domain = "discovery.refresh",
        service_type = DISCOVERY_SERVICE_TYPE,
        port = args.port,
        local_hostname = %local_hostname,
        local_name = %local_name,
        local_peer_id = %local_peer_id,
        snapshot_interval_secs = args.snapshot_interval.as_secs(),
        run_for_secs = args.run_for.map(|duration| duration.as_secs()),
        "discovery smoke started"
    );

    println!(
        "discovery-smoke: advertising name={} hostname={} port={} service={} (Ctrl+C to stop)",
        local_name, local_hostname, args.port, DISCOVERY_SERVICE_TYPE
    );

    let started_at = Instant::now();
    let mut last_snapshot_at = started_at;
    let mut visible_peers: HashMap<String, SeenPeer> = HashMap::new();

    while running.load(Ordering::SeqCst) {
        if let Some(run_for) = args.run_for {
            if started_at.elapsed() >= run_for {
                info!(
                    target: DISCOVERY_LOG_TARGET,
                    domain = "discovery.refresh",
                    run_for_secs = run_for.as_secs(),
                    "discovery smoke reached requested runtime"
                );
                break;
            }
        }

        match rx.try_recv() {
            Ok(event) => handle_event(&mut visible_peers, event),
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => {
                warn!(
                    target: DISCOVERY_LOG_TARGET,
                    domain = "discovery.health",
                    "discovery event channel disconnected"
                );
                break;
            }
        }

        if last_snapshot_at.elapsed() >= args.snapshot_interval {
            log_snapshot(&visible_peers);
            last_snapshot_at = Instant::now();
        }

        thread::sleep(POLL_INTERVAL);
    }

    discovery.stop();

    log_snapshot(&visible_peers);
    info!(
        target: DISCOVERY_LOG_TARGET,
        domain = "discovery.refresh",
        uptime_ms = started_at.elapsed().as_millis() as u64,
        visible_peer_count = visible_peers.len(),
        "discovery smoke stopped"
    );

    Ok(())
}

fn handle_event(visible_peers: &mut HashMap<String, SeenPeer>, event: DiscoveryEvent) {
    match event {
        DiscoveryEvent::Discovered(peer) => {
            let now = Instant::now();
            match visible_peers.entry(peer.peer_id.clone()) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    let previous_hostname = existing.hostname.clone();
                    let previous_primary_addr = existing.primary_addr;
                    let previous_addrs = existing.addrs.clone();
                    existing.primary_addr = peer.primary_addr;
                    existing.addrs = peer.addrs.clone();
                    existing.hostname = peer.hostname.clone();
                    existing.last_seen = now;
                    existing.discoveries += 1;

                    if previous_hostname != peer.hostname
                        || previous_primary_addr != peer.primary_addr
                        || previous_addrs != peer.addrs
                    {
                        info!(
                            target: DISCOVERY_LOG_TARGET,
                            domain = "discovery.peer",
                            change = "updated",
                            peer_id = %peer.peer_id,
                            primary_addr = %peer.primary_addr,
                            previous_addr = %previous_primary_addr,
                            candidate_addrs = ?peer.addrs,
                            previous_hostname = %previous_hostname,
                            peer_hostname = %peer.hostname,
                            discoveries = existing.discoveries,
                            "discovered peer metadata changed"
                        );
                    } else {
                        debug!(
                            target: DISCOVERY_LOG_TARGET,
                            domain = "discovery.peer",
                            change = "refreshed",
                            peer_id = %peer.peer_id,
                            primary_addr = %peer.primary_addr,
                            peer_hostname = %peer.hostname,
                            discoveries = existing.discoveries,
                            "peer discovery refreshed"
                        );
                    }
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(SeenPeer {
                        primary_addr: peer.primary_addr,
                        addrs: peer.addrs.clone(),
                        hostname: peer.hostname.clone(),
                        first_seen: now,
                        last_seen: now,
                        discoveries: 1,
                    });
                    info!(
                        target: DISCOVERY_LOG_TARGET,
                        domain = "discovery.peer",
                        change = "added",
                        peer_id = %peer.peer_id,
                        primary_addr = %peer.primary_addr,
                        candidate_addrs = ?peer.addrs,
                        peer_hostname = %peer.hostname,
                        "peer discovered"
                    );
                }
            }
        }
        DiscoveryEvent::Removed { fullname, peer_id } => {
            if let Some(peer) = visible_peers.remove(&peer_id) {
                info!(
                    target: DISCOVERY_LOG_TARGET,
                    domain = "discovery.peer",
                    change = "removed",
                    fullname = %fullname,
                    peer_id = %peer_id,
                    primary_addr = %peer.primary_addr,
                    peer_hostname = %peer.hostname,
                    visible_for_ms = peer.first_seen.elapsed().as_millis() as u64,
                    discoveries = peer.discoveries,
                    "peer removed from discovery"
                );
            } else {
                warn!(
                    target: DISCOVERY_LOG_TARGET,
                    domain = "discovery.peer",
                    change = "removed",
                    fullname = %fullname,
                    peer_id = %peer_id,
                    "peer removed before it was tracked locally"
                );
            }
        }
    }
}

fn log_snapshot(visible_peers: &HashMap<String, SeenPeer>) {
    let mut peers = visible_peers
        .iter()
        .map(|(peer_id, peer)| {
            format!(
                "{}@{}({:?}) peer_id={} last_seen_ms={} discoveries={}",
                peer.hostname,
                peer.primary_addr,
                peer.addrs,
                peer_id,
                peer.last_seen.elapsed().as_millis(),
                peer.discoveries
            )
        })
        .collect::<Vec<_>>();
    peers.sort();

    info!(
        target: DISCOVERY_LOG_TARGET,
        domain = "discovery.refresh",
        visible_peer_count = visible_peers.len(),
        peers = %format!("[{}]", peers.join(", ")),
        "discovery peer snapshot"
    );
}

fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|hostname| hostname.into_string().ok())
        .map(|hostname| hostname.trim().to_string())
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or_else(|| format!("mouse-thing-{}", process::id()))
}

fn local_peer_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn parse_args_from<I>(args: I, program_name: &str) -> Result<ParsedArgs>
where
    I: IntoIterator<Item = String>,
{
    let mut parsed = Args::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help(usage(program_name))),
            "--port" => {
                parsed.port = parse_value::<u16>(args.next(), "--port")?;
            }
            "--name" => {
                let value = next_arg(args.next(), "--name")?;
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    bail!("--name must not be empty");
                }
                parsed.name = Some(trimmed.to_string());
            }
            "--snapshot-interval-secs" => {
                let seconds = parse_value::<u64>(args.next(), "--snapshot-interval-secs")?;
                if seconds == 0 {
                    bail!("--snapshot-interval-secs must be greater than 0");
                }
                parsed.snapshot_interval = Duration::from_secs(seconds);
            }
            "--run-for-secs" => {
                let seconds = parse_value::<u64>(args.next(), "--run-for-secs")?;
                if seconds == 0 {
                    bail!("--run-for-secs must be greater than 0");
                }
                parsed.run_for = Some(Duration::from_secs(seconds));
            }
            unknown => bail!("unknown argument: {unknown}\n\n{}", usage(program_name)),
        }
    }

    Ok(ParsedArgs::Run(parsed))
}

fn parse_value<T>(value: Option<String>, flag: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = next_arg(value, flag)?;
    raw.parse::<T>()
        .map_err(|error| anyhow::anyhow!("{flag} expects a valid value: {error}"))
}

fn next_arg(value: Option<String>, flag: &str) -> Result<String> {
    value.ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

fn usage(program_name: &str) -> String {
    format!(
        "\
Discovery-only smoke test for mouse-thing mDNS.

Usage:
  {program_name} [--port <udp-port>] [--name <display-name>] [--snapshot-interval-secs <n>] [--run-for-secs <n>]

Options:
  --port <udp-port>                UDP port to advertise and browse on (default: {DEFAULT_PORT})
  --name <display-name>            Display name to advertise (default: local hostname)
  --snapshot-interval-secs <n>     How often to emit a peer inventory snapshot (default: {DEFAULT_SNAPSHOT_INTERVAL_SECS})
  --run-for-secs <n>               Exit automatically after N seconds instead of waiting for Ctrl+C
  -h, --help                       Show this help text

Examples:
  cargo run --bin discovery-smoke --
  cargo run --bin discovery-smoke -- --name machine-a --snapshot-interval-secs 2
  cargo run --bin discovery-smoke -- --port 54321 --run-for-secs 30

Environment:
  MOUSE_THING_LOG_PRESET=debug     Includes peer refresh diagnostics in addition to add/remove events
  RUST_LOG=...                     Overrides the preset filter completely
"
    )
}

#[cfg(test)]
mod tests {
    use super::{Args, ParsedArgs, parse_args_from};
    use std::time::Duration;

    #[test]
    fn parse_defaults() {
        let parsed =
            parse_args_from(Vec::<String>::new(), "discovery-smoke").expect("parse should succeed");
        assert_eq!(
            parsed,
            ParsedArgs::Run(Args {
                port: 54321,
                name: None,
                snapshot_interval: Duration::from_secs(5),
                run_for: None,
            })
        );
    }

    #[test]
    fn parse_all_supported_flags() {
        let parsed = parse_args_from(
            vec![
                "--port".to_string(),
                "12345".to_string(),
                "--name".to_string(),
                "machine-a".to_string(),
                "--snapshot-interval-secs".to_string(),
                "2".to_string(),
                "--run-for-secs".to_string(),
                "30".to_string(),
            ],
            "discovery-smoke",
        )
        .expect("parse should succeed");

        assert_eq!(
            parsed,
            ParsedArgs::Run(Args {
                port: 12345,
                name: Some("machine-a".to_string()),
                snapshot_interval: Duration::from_secs(2),
                run_for: Some(Duration::from_secs(30)),
            })
        );
    }

    #[test]
    fn parse_help() {
        let parsed = parse_args_from(vec!["--help".to_string()], "discovery-smoke")
            .expect("help should succeed");
        match parsed {
            ParsedArgs::Help(help) => assert!(help.contains("Discovery-only smoke test")),
            ParsedArgs::Run(_) => panic!("expected help output"),
        }
    }

    #[test]
    fn parse_rejects_zero_snapshot_interval() {
        let error = parse_args_from(
            vec!["--snapshot-interval-secs".to_string(), "0".to_string()],
            "discovery-smoke",
        )
        .expect_err("parse should fail");

        assert!(error.to_string().contains("greater than 0"));
    }
}
