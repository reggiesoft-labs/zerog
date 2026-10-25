use std::fmt;
use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use std::collections::HashMap;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Registry};

pub const LOG_PRESET_ENV_VAR: &str = "MOUSE_THING_LOG_PRESET";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogPreset {
    Normal,
    Debug,
    Trace,
}

impl LogPreset {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }

    pub fn filter_directive(self) -> &'static str {
        match self {
            Self::Normal => "warn,mouse_thing=info,mouse_thing::core::discovery=info,mouse_thing::core::services::discovery=info",
            Self::Debug => {
                "warn,mouse_thing=info,mouse_thing::platform::shared=debug,mouse_thing::core::discovery=debug,mouse_thing::core::services::discovery=debug"
            }
            Self::Trace => {
                "warn,mouse_thing=info,mouse_thing::platform::shared=debug,mouse_thing::platform::shared::runtime_threads=trace,mouse_thing::platform::shared::transition=trace,mouse_thing::core::discovery=trace,mouse_thing::core::services::discovery=trace"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDiscoveryLoggingConfig {
    pub preset: LogPreset,
    pub filter_directive: String,
    pub ansi_enabled: bool,
}

impl RuntimeDiscoveryLoggingConfig {
    pub fn from_parts(
        preset_env: Option<&str>,
        rust_log_env: Option<&str>,
        stderr_is_tty: bool,
        ci_env_present: bool,
        test_sink: bool,
    ) -> Self {
        let preset = preset_env
            .and_then(LogPreset::parse)
            .unwrap_or(LogPreset::Normal);

        let filter_directive = rust_log_env
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| preset.filter_directive().to_string());

        Self {
            preset,
            filter_directive,
            ansi_enabled: should_enable_ansi(stderr_is_tty, ci_env_present, test_sink),
        }
    }

    pub fn from_env() -> Self {
        Self::from_parts(
            std::env::var(LOG_PRESET_ENV_VAR).ok().as_deref(),
            std::env::var("RUST_LOG").ok().as_deref(),
            std::io::stderr().is_terminal(),
            std::env::var_os("CI").is_some(),
            cfg!(test),
        )
    }
}

pub fn should_enable_ansi(stderr_is_tty: bool, ci_env_present: bool, test_sink: bool) -> bool {
    stderr_is_tty && !ci_env_present && !test_sink
}

pub fn install_panic_hook() {
    static PANIC_HOOK_INSTALLED: std::sync::Once = std::sync::Once::new();
    PANIC_HOOK_INSTALLED.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown location".to_string());
            let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "Box<dyn Any>".to_string()
            };
            let thread = std::thread::current();
            let thread_name = thread.name().unwrap_or("<unnamed>");

            let _ = writeln!(
                std::io::stderr(),
                "[PANIC] thread '{thread_name}' panicked at '{location}': {payload}"
            );
            tracing::error!(
                thread = %thread_name,
                location = %location,
                payload = %payload,
                "panic occurred in worker thread"
            );
            default_hook(info);
        }));
    });
}

pub fn init_runtime_discovery_logging() {
    let config = RuntimeDiscoveryLoggingConfig::from_env();
    let _ = tracing::subscriber::set_global_default(subscriber_for_config(&config));
    install_panic_hook();
}

#[derive(Debug, Clone, Copy)]
struct RuntimeDiscoveryFormat {
    preset: LogPreset,
}

impl<S, N> FormatEvent<S, N> for RuntimeDiscoveryFormat
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let mut fields = EventFields::default();
        event.record(&mut fields);

        if !should_emit_event(self.preset, metadata, &fields) {
            return Ok(());
        }

        let timestamp = current_timestamp();
        let level = metadata.level();
        let domain = fields
            .domain
            .as_deref()
            .unwrap_or_else(|| default_domain(metadata.target()));
        let message = fields.message.as_deref().unwrap_or(metadata.name());

        write!(writer, "{timestamp} {level:<5} {domain} {message}")?;
        for (key, value) in fields.values {
            write!(writer, " {key}={value}")?;
        }
        writeln!(writer)
    }
}

#[derive(Debug, Default)]
struct EventFields {
    message: Option<String>,
    domain: Option<String>,
    values: Vec<(String, String)>,
}

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let rendered = format!("{value:?}");
        match field.name() {
            "message" => self.message = Some(rendered.trim_matches('"').to_string()),
            "domain" => self.domain = Some(rendered.trim_matches('"').to_string()),
            _ => self.values.push((field.name().to_string(), rendered)),
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = Some(value.to_string()),
            "domain" => self.domain = Some(value.to_string()),
            _ => self
                .values
                .push((field.name().to_string(), format!("\"{value}\""))),
        }
    }
}

fn current_timestamp() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("{}.{:03}", duration.as_secs(), duration.subsec_millis()),
        Err(_) => "0.000".to_string(),
    }
}

fn default_domain(target: &str) -> &'static str {
    if target.contains("services::discovery") {
        return "discovery.refresh";
    }
    if target.contains("discovery_helpers") {
        return "discovery.peer";
    }
    if target.contains("entrypoints") {
        return "runtime.connection";
    }
    if target.contains("runtime_threads") || target.contains("transition") {
        return "runtime.transition";
    }
    if target.contains("quic") {
        return "runtime.connection";
    }
    "runtime.health"
}

fn should_emit_event(
    preset: LogPreset,
    metadata: &tracing::Metadata<'_>,
    fields: &EventFields,
) -> bool {
    if preset == LogPreset::Trace {
        return true;
    }

    let level = *metadata.level();
    if level == tracing::Level::ERROR {
        return true;
    }

    let message = fields.message.as_deref().unwrap_or(metadata.name());
    let domain = fields
        .domain
        .as_deref()
        .unwrap_or_else(|| default_domain(metadata.target()));

    if matches!(
        message,
        "peer heartbeat timeout"
            | "clipboard content changed, broadcasting"
            | "applied remote clipboard content"
            | "NSPasteboard content set"
    ) {
        return false;
    }

    if level == tracing::Level::WARN && message == "peer heartbeat timeout" {
        return false;
    }

    match preset {
        LogPreset::Normal => {
            if level == tracing::Level::WARN {
                return true;
            }

            if domain == "runtime.health" {
                return false;
            }

            true
        }
        LogPreset::Debug => {
            if level == tracing::Level::INFO && domain == "runtime.health" {
                return false;
            }

            true
        }
        LogPreset::Trace => true,
    }
}

pub fn subscriber_for_config(
    config: &RuntimeDiscoveryLoggingConfig,
) -> impl Subscriber + Send + Sync {
    subscriber_for_config_with_writer(config, std::io::stderr)
}

pub fn subscriber_for_config_with_writer<W>(
    config: &RuntimeDiscoveryLoggingConfig,
    writer: W,
) -> impl Subscriber + Send + Sync
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = EnvFilter::new(config.filter_directive.clone());
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .log_internal_errors(false)
        .with_ansi(config.ansi_enabled)
        .event_format(RuntimeDiscoveryFormat {
            preset: config.preset,
        });
    Registry::default().with(filter).with(fmt_layer)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollupLevel {
    Info,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitPolicy {
    pub interval_ms: u64,
    pub burst: u32,
    pub rollup_level: RollupLevel,
}

impl RateLimitPolicy {
    pub const fn new(interval_ms: u64, burst: u32, rollup_level: RollupLevel) -> Self {
        Self {
            interval_ms,
            burst,
            rollup_level,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitRollup {
    pub suppressed: u32,
    pub level: RollupLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitDecision {
    pub emit_now: bool,
    pub suppressed_since_last_emit: u32,
    pub rollup: Option<RateLimitRollup>,
}

#[derive(Debug, Default)]
pub struct LogRateLimiter {
    state: Mutex<HashMap<String, RateLimitState>>,
}

#[derive(Debug, Clone, Copy)]
struct RateLimitState {
    window_start_ms: u64,
    emitted: u32,
    suppressed: u32,
}

impl LogRateLimiter {
    pub fn evaluate(&self, key: &str, policy: RateLimitPolicy) -> RateLimitDecision {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.evaluate_at(key, policy, now_ms)
    }

    pub fn evaluate_at(
        &self,
        key: &str,
        policy: RateLimitPolicy,
        now_ms: u64,
    ) -> RateLimitDecision {
        let mut state = self.state.lock().expect("rate limiter lock poisoned");
        let entry = state
            .entry(key.to_string())
            .or_insert_with(|| RateLimitState {
                window_start_ms: now_ms,
                emitted: 0,
                suppressed: 0,
            });

        let mut rollup = None;
        if now_ms.saturating_sub(entry.window_start_ms) >= policy.interval_ms {
            if entry.suppressed > 0 {
                rollup = Some(RateLimitRollup {
                    suppressed: entry.suppressed,
                    level: policy.rollup_level,
                });
            }
            entry.window_start_ms = now_ms;
            entry.emitted = 0;
            entry.suppressed = 0;
        }

        if entry.emitted < policy.burst {
            let suppressed_since_last_emit = entry.suppressed;
            entry.emitted += 1;
            entry.suppressed = 0;
            RateLimitDecision {
                emit_now: true,
                suppressed_since_last_emit,
                rollup,
            }
        } else {
            entry.suppressed = entry.suppressed.saturating_add(1);
            RateLimitDecision {
                emit_now: false,
                suppressed_since_last_emit: 0,
                rollup,
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn logging_survives_closed_stderr() {
        const CHILD_ENV: &str = "MOUSE_THING_CLOSED_STDERR_TEST";
        if std::env::var_os(CHILD_ENV).is_some() {
            super::init_runtime_discovery_logging();
            tracing::info!("logging after the dev launcher closes stderr");
            let panic = std::panic::catch_unwind(|| {
                panic!("worker panic after the dev launcher closes stderr");
            });
            assert!(panic.is_err());
            return;
        }

        let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(reader);
        let stderr: std::os::fd::OwnedFd = writer.into();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "logging::tests::logging_survives_closed_stderr",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(stderr))
            .status()
            .unwrap();

        assert!(status.success(), "closed stderr must not abort: {status}");
    }
}
