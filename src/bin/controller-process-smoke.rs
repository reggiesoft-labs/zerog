use anyhow::{Context, Result, bail};
use mouse_thing::core::{Controller, CoreError};
use mouse_thing::platform::shared::test_support::create_test_engine_pair;
use std::env;
use std::io::Write;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    port: u16,
    connect_to: Option<SocketAddr>,
    run_for: Duration,
    exit_after_disconnect: bool,
}

enum ParsedArgs {
    Run(Args),
    Help,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            port: 0,
            connect_to: None,
            run_for: Duration::from_secs(30),
            exit_after_disconnect: false,
        }
    }
}

fn main() -> Result<()> {
    mouse_thing::install_crypto_provider();
    let args = match parse_args(env::args().skip(1))? {
        ParsedArgs::Run(args) => args,
        ParsedArgs::Help => {
            println!(
                "controller-process-smoke [--port <port>] [--connect-to <ip:port>] [--run-for-secs <seconds>] [--exit-after-disconnect]"
            );
            return Ok(());
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    runtime.block_on(run(args))
}

async fn run(args: Args) -> Result<()> {
    let source_marker = u64::from(std::process::id());
    let mut controller = Controller::with_engine_pair(move |peer_edge| {
        create_test_engine_pair(
            "controller-process-smoke",
            "Controller Process Smoke",
            peer_edge,
            0,
            source_marker,
        )
        .map_err(|error| CoreError::Engine(error.to_string()))
    });

    controller
        .start(args.port)
        .with_context(|| format!("failed to start controller on port {}", args.port))?;
    announce(&format!("READY port={}", args.port));

    let mut update_rx = controller.subscribe_to_peer_updates();
    let started_at = Instant::now();
    let mut connected = false;
    let mut connection_generation = 0;
    let mut last_connect_attempt = Instant::now() - Duration::from_secs(1);

    while started_at.elapsed() < args.run_for {
        if let Some(rx) = update_rx.as_mut() {
            while let Ok(update) = rx.try_recv() {
                if let mouse_thing::core::controller::events::PeerUpdate::PairingRequested { peer_id, .. } = update {
                    let _ = controller.approve_pairing(&peer_id);
                }
            }
        }

        let is_connected = !controller.get_status().connected_peers.is_empty();

        if is_connected && !connected {
            connection_generation += 1;
            announce(&format!("CONNECTED generation={connection_generation}"));
        } else if !is_connected && connected {
            announce("DISCONNECTED");
            if args.exit_after_disconnect {
                break;
            }
        }
        connected = is_connected;

        if !is_connected
            && let Some(addr) = args.connect_to
            && last_connect_attempt.elapsed() >= Duration::from_millis(200)
        {
            let _ = controller.connect_address(addr);
            last_connect_attempt = Instant::now();
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    announce("STOPPING");
    controller.stop();
    announce("STOPPED");
    Ok(())
}

fn announce(message: &str) {
    println!("{message}");
    let _ = std::io::stdout().flush();
}

fn parse_args<I>(args: I) -> Result<ParsedArgs>
where
    I: IntoIterator<Item = String>,
{
    let mut parsed = Args::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => parsed.port = parse_value(args.next(), "--port")?,
            "--connect-to" => parsed.connect_to = Some(parse_value(args.next(), "--connect-to")?),
            "--run-for-secs" => {
                let seconds: u64 = parse_value(args.next(), "--run-for-secs")?;
                if seconds == 0 {
                    bail!("--run-for-secs must be greater than 0");
                }
                parsed.run_for = Duration::from_secs(seconds);
            }
            "--exit-after-disconnect" => parsed.exit_after_disconnect = true,
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            unknown => bail!("unknown argument: {unknown}"),
        }
    }

    Ok(ParsedArgs::Run(parsed))
}

fn parse_value<T>(value: Option<String>, flag: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = value.ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))?;
    value
        .parse::<T>()
        .map_err(|error| anyhow::anyhow!("{flag} expects a valid value: {error}"))
}
