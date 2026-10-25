use anyhow::{Context, Result, anyhow, bail};
use mouse_thing::platform::shared::edge_probe::EdgeProbe;
use mouse_thing::platform::{DisplayBounds, PeerEdge, create_engine_pair};
use std::env;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_INTERVAL_MS: u64 = 8;
const TOPOLOGY_REFRESH_INTERVAL_MS: u64 = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    interval: Duration,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(DEFAULT_INTERVAL_MS),
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
        .unwrap_or_else(|| "edge-smoke".to_string());
    let args = match parse_args_from(env::args().skip(1), &program_name)? {
        ParsedArgs::Run(args) => args,
        ParsedArgs::Help(help) => {
            println!("{help}");
            return Ok(());
        }
    };

    let running = Arc::new(AtomicBool::new(true));
    let signal_running = Arc::clone(&running);
    ctrlc::set_handler(move || {
        signal_running.store(false, Ordering::SeqCst);
    })
    .context("failed to install Ctrl+C handler")?;

    let (platform, _session) = create_engine_pair(PeerEdge::Right)?;
    let display = platform.get_display_manager();
    let cursor_source = CursorSource::new()?;
    let mut displays = display.discover_displays()?;
    let mut display_signature = display.get_signature()?;
    let mut probe = EdgeProbe::new(displays.clone());
    let mut last_topology_refresh = Instant::now();
    let started_at = Instant::now();

    print_banner(args.interval);
    print_topology(&displays, probe.virtual_bounds());

    while running.load(Ordering::SeqCst) {
        if last_topology_refresh.elapsed() >= Duration::from_millis(TOPOLOGY_REFRESH_INTERVAL_MS) {
            let next_signature = display.get_signature()?;
            if next_signature != display_signature {
                displays = display.discover_displays()?;
                display_signature = next_signature;
                probe.update_displays(displays.clone());
                println!("topology changed");
                print_topology(&displays, probe.virtual_bounds());
            }
            last_topology_refresh = Instant::now();
        }

        let (cursor_x, cursor_y) = cursor_source.current_cursor_position()?;
        let sample = probe.sample(
            cursor_x,
            cursor_y,
            started_at.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
        );
        if sample.dwell_elapsed {
            let edge = sample.edge.expect("dwell event should have an edge");
            println!(
                "edge-dwell: edge={} cursor=({}, {}) elapsed_ms={}",
                edge.as_str(),
                cursor_x,
                cursor_y,
                started_at.elapsed().as_millis()
            );
        }

        thread::sleep(args.interval);
    }

    println!("edge-smoke: stopped");
    Ok(())
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
            "--interval-ms" => {
                let value = next_arg(args.next(), "--interval-ms")?;
                let millis = value
                    .parse::<u64>()
                    .with_context(|| format!("invalid --interval-ms value: {value}"))?;
                if millis == 0 {
                    bail!("--interval-ms must be greater than 0");
                }
                parsed.interval = Duration::from_millis(millis);
            }
            other => bail!("unknown argument: {other}\n\n{}", usage(program_name)),
        }
    }

    Ok(ParsedArgs::Run(parsed))
}

fn next_arg(value: Option<String>, flag: &str) -> Result<String> {
    value.ok_or_else(|| anyhow!("missing value for {flag}"))
}

fn usage(program_name: &str) -> String {
    format!(
        "Usage: {program_name} [--interval-ms N]\n\n\
         Polls the real cursor position, evaluates the shared outer-edge dwell detector,\n\
         and prints a line when the cursor dwells at a display-edge segment.\n\n\
         Options:\n\
           --interval-ms N   Poll interval in milliseconds (default: {DEFAULT_INTERVAL_MS})\n\
           -h, --help        Show this help text"
    )
}

fn print_banner(interval: Duration) {
    println!(
        "edge-smoke: pid={} interval_ms={} refresh_ms={} (Ctrl+C to stop)",
        process::id(),
        interval.as_millis(),
        TOPOLOGY_REFRESH_INTERVAL_MS
    );
}

fn print_topology(displays: &[DisplayBounds], virtual_bounds: DisplayBounds) {
    println!(
        "topology: virtual={} displays={}",
        format_bounds(virtual_bounds),
        displays
            .iter()
            .enumerate()
            .map(|(index, display)| format!("#{index}:{}", format_bounds(*display)))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn format_bounds(bounds: DisplayBounds) -> String {
    format!(
        "x={} y={} w={} h={}",
        bounds.x, bounds.y, bounds.width, bounds.height
    )
}

#[cfg(target_os = "macos")]
struct CursorSource;

#[cfg(target_os = "macos")]
impl CursorSource {
    fn new() -> Result<Self> {
        Ok(Self)
    }

    fn current_cursor_position(&self) -> Result<(i32, i32)> {
        use core_graphics::event::CGEvent;
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|_| anyhow!("failed to create CGEventSource"))?;
        let event = CGEvent::new(source).map_err(|_| anyhow!("failed to read current cursor"))?;
        let point = event.location();
        Ok((point.x.round() as i32, point.y.round() as i32))
    }
}

#[cfg(target_os = "windows")]
struct CursorSource;

#[cfg(target_os = "windows")]
impl CursorSource {
    fn new() -> Result<Self> {
        Ok(Self)
    }

    fn current_cursor_position(&self) -> Result<(i32, i32)> {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

        let mut point = POINT { x: 0, y: 0 };
        let ok = unsafe { GetCursorPos(&mut point) };
        if ok == 0 {
            bail!("GetCursorPos failed");
        }
        Ok((point.x, point.y))
    }
}

#[cfg(target_os = "linux")]
struct CursorSource {
    conn: x11rb::rust_connection::RustConnection,
    root: u32,
}

#[cfg(target_os = "linux")]
impl CursorSource {
    fn new() -> Result<Self> {
        use x11rb::connection::Connection;

        if env::var_os("DISPLAY").is_none() {
            bail!("edge-smoke requires an X11/XWayland DISPLAY on Linux");
        }

        let (conn, screen_num) = x11rb::connect(None).context("failed to connect to X11")?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;
        Ok(Self { conn, root })
    }

    fn current_cursor_position(&self) -> Result<(i32, i32)> {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;

        let reply = self
            .conn
            .query_pointer(self.root)
            .context("failed to query X11 pointer")?
            .reply()
            .context("failed to read X11 pointer reply")?;
        Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
    }
}
