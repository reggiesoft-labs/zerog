use anyhow::{Context, Result, bail};
use mouse_thing::core::{Controller, CoreError, PeerSnapshot};
use std::env;
use std::process;
use std::time::{Duration, Instant};
use tracing::info;

const DEFAULT_PORT: u16 = mouse_thing::core::policies::DEFAULT_PORT;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_SNAPSHOT_INTERVAL_SECS: u64 = 2;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const CONNECT_LOG_TARGET: &str = "mouse_thing::platform::shared::entrypoints";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    port: u16,
    peer: Option<String>,
    address: Option<std::net::SocketAddr>,
    timeout: Duration,
    snapshot_interval: Duration,
    hold_for: Option<Duration>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            peer: None,
            address: None,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            snapshot_interval: Duration::from_secs(DEFAULT_SNAPSHOT_INTERVAL_SECS),
            hold_for: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedArgs {
    Run(Args),
    Help(String),
}

fn main() -> Result<()> {
    let program_name = env::args()
        .next()
        .unwrap_or_else(|| "connect-smoke".to_string());
    let args = match parse_args_from(env::args().skip(1), &program_name)? {
        ParsedArgs::Run(args) => args,
        ParsedArgs::Help(help) => {
            println!("{help}");
            return Ok(());
        }
    };

    mouse_thing::install_crypto_provider();
    mouse_thing::logging::init_runtime_discovery_logging();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    let mut controller = Controller::with_engine_pair(|peer_edge| {
        mouse_thing::platform::create_engine_pair(peer_edge)
            .map_err(|error| CoreError::Engine(error.to_string()))
    });
    let connect_result = runtime.block_on(run_connect_smoke(&mut controller, &args));
    controller.stop();
    connect_result
}

async fn run_connect_smoke(controller: &mut Controller, args: &Args) -> Result<()> {
    controller
        .start(args.port)
        .with_context(|| format!("failed to start controller on port {}", args.port))?;

    if let Some(addr) = args.address {
        controller
            .connect_address(addr)
            .with_context(|| format!("failed to dial direct address {}", addr))?;
    }

    let local_hostname = local_hostname();
    let peer_filter = args.peer.as_deref().unwrap_or("<any>");

    info!(
        target: CONNECT_LOG_TARGET,
        domain = "runtime.connection",
        port = args.port,
        peer_filter = %peer_filter,
        direct_address = ?args.address,
        timeout_secs = args.timeout.as_secs(),
        hold_for_secs = args.hold_for.map(|duration| duration.as_secs()),
        "connect smoke started"
    );

    println!(
        "connect-smoke: local_hostname={} port={} peer_filter={} direct_address={:?} timeout={}s",
        local_hostname,
        args.port,
        peer_filter,
        args.address,
        args.timeout.as_secs(),
    );

    let started_at = Instant::now();
    let mut last_snapshot_at = started_at
        .checked_sub(args.snapshot_interval)
        .unwrap_or(started_at);

    loop {
        let connected_peers = controller.get_peers_cached();
        if let Some(peer) = select_peer(&connected_peers, args.peer.as_deref()) {
            print_success(peer, started_at.elapsed());
            info!(
                target: CONNECT_LOG_TARGET,
                domain = "runtime.connection",
                peer_id = %peer.peer_id,
                peer_name = %peer.name,
                peer_hostname = %peer.hostname,
                peer_addr = %peer.addr,
                connected_after_ms = started_at.elapsed().as_millis() as u64,
                "connect smoke reached connected state"
            );

            if let Some(hold_for) = args.hold_for {
                println!("holding connection open for {}s", hold_for.as_secs());
                tokio::time::sleep(hold_for).await;
            }

            return Ok(());
        }

        if started_at.elapsed() >= args.timeout {
            bail!(
                "timed out after {}s waiting for peer {} to connect",
                args.timeout.as_secs(),
                peer_filter
            );
        }

        if last_snapshot_at.elapsed() >= args.snapshot_interval {
            print_snapshot(&connected_peers, args.peer.as_deref());
            last_snapshot_at = Instant::now();
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn select_peer<'a>(peers: &'a [PeerSnapshot], filter: Option<&str>) -> Option<&'a PeerSnapshot> {
    peers
        .iter()
        .filter(|peer| !peer.is_local && peer.connected)
        .find(|peer| match filter {
            Some(filter) => peer_matches_filter(peer, filter),
            None => true,
        })
}

fn peer_matches_filter(peer: &PeerSnapshot, filter: &str) -> bool {
    let filter = filter.trim();
    if filter.is_empty() {
        return true;
    }

    peer.peer_id == filter || peer.hostname == filter || peer.name == filter
}

fn print_success(peer: &PeerSnapshot, elapsed: Duration) {
    println!(
        "connected: name={} hostname={} peer_id={} addr={} after={}ms",
        peer.name,
        peer.hostname,
        peer.peer_id,
        peer.addr,
        elapsed.as_millis()
    );
}

fn print_snapshot(peers: &[PeerSnapshot], filter: Option<&str>) {
    let filter = filter.unwrap_or("<any>");
    let mut entries = peers
        .iter()
        .map(|peer| format!("{}:{}@{}", peer.name, peer.hostname, peer.addr))
        .collect::<Vec<_>>();
    entries.sort();

    println!(
        "connected_peers(filter={}): {}",
        filter,
        if entries.is_empty() {
            "<none>".to_string()
        } else {
            entries.join(", ")
        }
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
            "--peer" => {
                let value = next_arg(args.next(), "--peer")?;
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    bail!("--peer must not be empty");
                }
                parsed.peer = Some(trimmed.to_string());
            }
            "--address" => {
                parsed.address = Some(parse_value::<std::net::SocketAddr>(args.next(), "--address")?);
            }
            "--timeout-secs" => {
                let seconds = parse_value::<u64>(args.next(), "--timeout-secs")?;
                if seconds == 0 {
                    bail!("--timeout-secs must be greater than 0");
                }
                parsed.timeout = Duration::from_secs(seconds);
            }
            "--snapshot-interval-secs" => {
                let seconds = parse_value::<u64>(args.next(), "--snapshot-interval-secs")?;
                if seconds == 0 {
                    bail!("--snapshot-interval-secs must be greater than 0");
                }
                parsed.snapshot_interval = Duration::from_secs(seconds);
            }
            "--hold-for-secs" => {
                let seconds = parse_value::<u64>(args.next(), "--hold-for-secs")?;
                if seconds == 0 {
                    bail!("--hold-for-secs must be greater than 0");
                }
                parsed.hold_for = Some(Duration::from_secs(seconds));
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
Connection smoke test for mouse-thing peer discovery and outbound connect.

Usage:
  {program_name} [--port <udp-port>] [--peer <hostname|name|peer-id>] [--address <ip:port>] [--timeout-secs <n>] [--snapshot-interval-secs <n>] [--hold-for-secs <n>]

Options:
  --port <udp-port>                UDP/QUIC port to listen on and discover on (default: {DEFAULT_PORT})
  --peer <hostname|name|peer-id>   Wait for a specific peer instead of the first connected peer
  --address <ip:port>              Directly dial a specific socket address without relying on mDNS
  --timeout-secs <n>               Fail if no matching peer connects within N seconds (default: {DEFAULT_TIMEOUT_SECS})
  --snapshot-interval-secs <n>     How often to print the current connected peer snapshot (default: {DEFAULT_SNAPSHOT_INTERVAL_SECS})
  --hold-for-secs <n>              Keep the connection open for N seconds after success before exiting
  -h, --help                       Show this help text

Examples:
  cargo run --bin connect-smoke --
  cargo run --bin connect-smoke -- --peer machine-b --timeout-secs 20
  cargo run --bin connect-smoke -- --address 192.168.1.50:54321
  cargo run --bin connect-smoke -- --peer peer-123 --hold-for-secs 10

Notes:
  This starts the real controller and platform engine, so host accessibility/input
  permissions may be required depending on the OS/backend.

Environment:
  MOUSE_THING_LOG_PRESET=debug     Includes runtime connection diagnostics
  RUST_LOG=...                     Overrides the preset filter completely
"
    )
}

#[cfg(test)]
mod tests {
    use super::{Args, ParsedArgs, parse_args_from, peer_matches_filter};
    use mouse_thing::core::PeerSnapshot;
    use std::time::Duration;

    #[test]
    fn parse_defaults() {
        let parsed =
            parse_args_from(Vec::<String>::new(), "connect-smoke").expect("parse should succeed");
        assert_eq!(
            parsed,
            ParsedArgs::Run(Args {
                port: 54321,
                peer: None,
                address: None,
                timeout: Duration::from_secs(30),
                snapshot_interval: Duration::from_secs(2),
                hold_for: None,
            })
        );
    }

    #[test]
    fn parse_all_supported_flags() {
        let parsed = parse_args_from(
            vec![
                "--port".to_string(),
                "12345".to_string(),
                "--peer".to_string(),
                "machine-a".to_string(),
                "--address".to_string(),
                "192.168.1.100:54321".to_string(),
                "--timeout-secs".to_string(),
                "20".to_string(),
                "--snapshot-interval-secs".to_string(),
                "3".to_string(),
                "--hold-for-secs".to_string(),
                "10".to_string(),
            ],
            "connect-smoke",
        )
        .expect("parse should succeed");

        assert_eq!(
            parsed,
            ParsedArgs::Run(Args {
                port: 12345,
                peer: Some("machine-a".to_string()),
                address: Some("192.168.1.100:54321".parse().unwrap()),
                timeout: Duration::from_secs(20),
                snapshot_interval: Duration::from_secs(3),
                hold_for: Some(Duration::from_secs(10)),
            })
        );
    }

    #[test]
    fn parse_help() {
        let parsed = parse_args_from(vec!["--help".to_string()], "connect-smoke")
            .expect("help should succeed");
        match parsed {
            ParsedArgs::Help(help) => assert!(help.contains("Connection smoke test")),
            ParsedArgs::Run(_) => panic!("expected help output"),
        }
    }

    #[test]
    fn peer_filter_matches_hostname_name_and_peer_id() {
        let peer = PeerSnapshot {
            peer_id: "peer-123".to_string(),
            name: "Machine B".to_string(),
            hostname: "machine-b".to_string(),
            addr: "127.0.0.1:54321".to_string(),
            platform: None,
            screen: None,
            virtual_bounds: None,
            display_rects: Vec::new(),
            last_seen_ms: 0,
            discovery_status: Default::default(),
            is_local: false,
            connected: false,
            paired: false,
            connection_status: None,
        };

        assert!(peer_matches_filter(&peer, "peer-123"));
        assert!(peer_matches_filter(&peer, "Machine B"));
        assert!(peer_matches_filter(&peer, "machine-b"));
        assert!(!peer_matches_filter(&peer, "machine-c"));
    }

    #[test]
    fn parse_address_only() {
        let parsed = parse_args_from(
            vec!["--address".to_string(), "10.0.0.5:12345".to_string()],
            "connect-smoke",
        )
        .expect("parse should succeed");

        assert_eq!(
            parsed,
            ParsedArgs::Run(Args {
                port: 54321,
                peer: None,
                address: Some("10.0.0.5:12345".parse().unwrap()),
                timeout: Duration::from_secs(30),
                snapshot_interval: Duration::from_secs(2),
                hold_for: None,
            })
        );
    }
}
