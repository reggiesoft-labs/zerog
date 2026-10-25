use anyhow::{Context, Result, bail};
use serial_test::serial;
use std::io::{BufRead, BufReader, Read};
use std::net::UdpSocket;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(15);

struct SmokeProcess {
    label: String,
    child: Child,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
    output: Vec<String>,
}

impl SmokeProcess {
    fn terminate(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }

    fn wait_for(&mut self, expected: &str) -> Result<()> {
        let started_at = Instant::now();
        while started_at.elapsed() < PROCESS_TIMEOUT {
            while let Ok(line) = self.stdout.try_recv() {
                self.output.push(line.clone());
                if line.contains(expected) {
                    return Ok(());
                }
            }

            if let Some(status) = self.child.try_wait()? {
                bail!(
                    "{} exited with {status} before {expected}; stdout: {}; stderr: {}",
                    self.label,
                    self.output.join(" | "),
                    drain(&self.stderr)
                );
            }

            thread::sleep(Duration::from_millis(25));
        }

        bail!(
            "timed out waiting for {} to emit {expected}; stdout: {}; stderr: {}",
            self.label,
            self.output.join(" | "),
            drain(&self.stderr)
        )
    }
}

impl Drop for SmokeProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[test]
#[serial]
fn controller_processes_connect_reconnect_and_shutdown() -> Result<()> {
    let home_a = tempfile::tempdir()?;
    let home_b = tempfile::tempdir()?;
    let port_a = reserve_port()?;
    let port_b = reserve_port()?;
    let addr_a = format!("127.0.0.1:{port_a}");

    let mut process_a = spawn_process("A", home_a.path(), port_a, None, false, &[])?;
    process_a.wait_for("READY")?;

    let mut process_b =
        spawn_process("B", home_b.path(), port_b, Some(&addr_a), false, &[])?;
    process_b.wait_for("READY")?;
    process_a.wait_for("CONNECTED generation=1")?;
    process_b.wait_for("CONNECTED generation=1")?;

    process_a.terminate();
    process_b.wait_for("DISCONNECTED")?;

    let mut restarted_a =
        spawn_process("A-restarted", home_a.path(), port_a, None, true, &[])?;
    restarted_a.wait_for("READY")?;
    restarted_a.wait_for("CONNECTED generation=1")?;
    process_b.wait_for("CONNECTED generation=2")?;

    process_b.terminate();
    restarted_a.wait_for("DISCONNECTED")?;
    restarted_a.wait_for("STOPPED")?;

    Ok(())
}

#[test]
#[serial]
#[ignore = "requires a host with working mDNS multicast"]
fn controller_processes_connect_over_mdns() -> Result<()> {
    let home_a = tempfile::tempdir()?;
    let home_b = tempfile::tempdir()?;
    let port_a = reserve_port()?;
    let port_b = reserve_port()?;
    let test_mdns = [
        ("ZEROG_SERVICE_TYPE", "_zerog-test._udp.local."),
        ("ZEROG_DISABLE_MDNS", "0"),
    ];

    let mut process_a = spawn_process("mDNS-A", home_a.path(), port_a, None, false, &test_mdns)?;
    process_a.wait_for("READY")?;
    let mut process_b = spawn_process("mDNS-B", home_b.path(), port_b, None, false, &test_mdns)?;
    process_b.wait_for("READY")?;

    process_a.wait_for("CONNECTED generation=1")?;
    process_b.wait_for("CONNECTED generation=1")?;

    process_a.terminate();
    process_b.terminate();
    Ok(())
}

fn spawn_process(
    label: &str,
    home: &Path,
    port: u16,
    connect_to: Option<&str>,
    exit_after_disconnect: bool,
    extra_env: &[(&str, &str)],
) -> Result<SmokeProcess> {
    let executable = option_env!("CARGO_BIN_EXE_controller-process-smoke")
        .context("Cargo did not provide the controller-process-smoke executable")?;
    let mut command = Command::new(executable);
    command
        .arg("--port")
        .arg(port.to_string())
        .arg("--run-for-secs")
        .arg("30")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home)
        .env("LOCALAPPDATA", home)
        .env("XDG_CONFIG_HOME", home)
        .env("ZEROG_DISABLE_MDNS", "1")
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, val) in extra_env {
        command.env(key, val);
    }

    if let Some(connect_to) = connect_to {
        command.arg("--connect-to").arg(connect_to);
    }
    if exit_after_disconnect {
        command.arg("--exit-after-disconnect");
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {label}"))?;
    let stdout = child
        .stdout
        .take()
        .context("controller process did not expose stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("controller process did not expose stderr")?;

    Ok(SmokeProcess {
        label: label.to_string(),
        child,
        stdout: spawn_reader(stdout),
        stderr: spawn_reader(stderr),
        output: Vec::new(),
    })
}

fn spawn_reader<R>(reader: R) -> Receiver<String>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn drain(receiver: &Receiver<String>) -> String {
    receiver.try_iter().collect::<Vec<_>>().join(" | ")
}

fn reserve_port() -> Result<u16> {
    Ok(UdpSocket::bind("127.0.0.1:0")?.local_addr()?.port())
}
