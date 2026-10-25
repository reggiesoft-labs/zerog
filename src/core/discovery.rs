#![cfg_attr(target_os = "macos", allow(dead_code, unused_imports))]
//! mDNS service discovery for peer-to-peer mouse sharing.
//!
//! This module provides lightweight mDNS-based discovery that simply reports
//! when a peer is available at a given address. It does NOT track peer state
//! or deduplicate by peer_id - that responsibility moves to the controller-layer
//! `PeerConnector` and [`Controller`](super::controller::Controller).

pub mod subnet_scanner;
#[cfg(target_os = "macos")]
mod macos;

use crate::core::policies;
use anyhow::{Context, Result, anyhow};
use flume::{Receiver, RecvTimeoutError};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::process;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;

const TXT_KEY_PEER_ID: &str = "peer_id";
const TXT_KEY_NAME: &str = "name";
const TXT_KEY_HOSTNAME: &str = "hostname";
const TXT_KEY_PLATFORM: &str = "platform";
const TXT_KEY_CERT_FINGERPRINT: &str = "cert_fingerprint";
const DISCOVERY_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Minimal peer info from discovery - just enough to initiate a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPeer {
    /// Stable peer identifier advertised in TXT records.
    pub peer_id: String,
    /// Primary network address for connections.
    pub primary_addr: SocketAddr,
    /// All candidate network addresses for connections.
    pub addrs: Vec<SocketAddr>,
    /// Hostname from TXT record or service info.
    pub hostname: String,
    /// Full mDNS service name for removal bookkeeping.
    pub fullname: String,
    /// Operating system platform string if advertised.
    pub platform: Option<String>,
}

/// Event emitted by mDNS discovery for peer availability.
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// A peer service was discovered at this address.
    Discovered(DiscoveredPeer),
    /// A peer service was removed from the network.
    Removed {
        /// The fullname of the removed service.
        fullname: String,
        /// Stable peer identifier associated with the removed service.
        peer_id: String,
    },
}

#[derive(Debug, Clone)]
struct ResolvedServicePeer {
    peer_id: String,
}

struct DiscoveryRuntime {
    daemon: ServiceDaemon,
    local_service_fullname: Option<String>,
    service_info: Option<ServiceInfo>,
    browse_rx: Receiver<ServiceEvent>,
    peers_by_service_fullname: HashMap<String, ResolvedServicePeer>,
}

impl Drop for DiscoveryRuntime {
    fn drop(&mut self) {
        if let Some(fullname) = &self.local_service_fullname {
            let _ = self.daemon.unregister(fullname);
        }
        if let Ok(monitor) = self.daemon.shutdown() {
            let _ = monitor.recv_timeout(Duration::from_millis(500));
        }
        tracing::debug!("discovery background loop ended");
    }
}

/// Push-based DiscoveryClient that runs mDNS discovery in a background thread.
pub struct DiscoveryClient {
    stop_tx: Option<std_mpsc::Sender<()>>,
    join_handle: Option<thread::JoinHandle<()>>,
    disabled_event_tx: Option<mpsc::Sender<DiscoveryEvent>>,
}

impl Default for DiscoveryClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DiscoveryClient {
    /// Creates a new empty DiscoveryClient with no background thread yet.
    ///
    /// Call `start()` to begin discovery and advertising.
    pub fn new() -> Self {
        Self {
            stop_tx: None,
            join_handle: None,
            disabled_event_tx: None,
        }
    }

    /// Starts the DiscoveryClient with the given parameters.
    ///
    /// This spawns a background thread that:
    /// - Creates a ServiceDaemon
    /// - Registers the local service via mDNS
    /// - Browses for peers on the network
    /// - Converts ServiceEvent → DiscoveryEvent and sends them via the returned Receiver
    ///
    /// # Arguments
    /// * `port` - The port to advertise for the local service
    /// * `display_name` - Optional display name for the local service
    /// * `peer_id` - Optional peer ID to include in advertisements
    /// * `cert_fingerprint` - Optional TLS certificate fingerprint
    ///
    /// # Returns
    /// A `mpsc::Receiver<DiscoveryEvent>` that will receive discovery events
    pub fn start(
        &mut self,
        port: u16,
        display_name: Option<&str>,
        peer_id: Option<&str>,
        cert_fingerprint: Option<&str>,
    ) -> Result<mpsc::Receiver<DiscoveryEvent>> {
        if self.join_handle.is_some() {
            self.stop();
        }

        let mdns_disabled = std::env::var("ZEROG_DISABLE_MDNS")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false);
        if mdns_disabled {
            tracing::debug!("mDNS discovery disabled via ZEROG_DISABLE_MDNS");
            let (event_tx, event_rx) = mpsc::channel(1);
            self.disabled_event_tx = Some(event_tx);
            return Ok(event_rx);
        }

        let (event_tx, event_rx) = mpsc::channel(100);
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let (startup_tx, startup_rx) = std_mpsc::sync_channel::<Result<()>>(1);

        let display_name: Option<String> = display_name.map(String::from);
        let peer_id: Option<String> = peer_id.map(String::from);
        let cert_fingerprint: Option<String> = cert_fingerprint.map(String::from);

        let handle = thread::spawn(move || {
            #[cfg(target_os = "macos")]
            {
                macos::run_native_discovery(
                    port,
                    display_name.as_deref(),
                    peer_id.as_deref(),
                    cert_fingerprint.as_deref(),
                    event_tx,
                    stop_rx,
                    startup_tx,
                );
                return;
            }

            #[cfg(not(target_os = "macos"))]
            match Self::initialize_runtime(
                port,
                display_name.as_deref(),
                peer_id.as_deref(),
                cert_fingerprint.as_deref(),
            ) {
                Ok(runtime) => {
                    let _ = startup_tx.send(Ok(()));
                    Self::background_loop(runtime, event_tx, stop_rx);
                }
                Err(error) => {
                    let _ = startup_tx.send(Err(anyhow!("{error:#}")));
                }
            }
        });

        match startup_rx.recv() {
            Ok(Ok(())) => {
                self.stop_tx = Some(stop_tx);
                self.join_handle = Some(handle);
                Ok(event_rx)
            }
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(_) => {
                let _ = handle.join();
                anyhow::bail!("Discovery background thread exited before startup completed");
            }
        }
    }

    fn service_type() -> String {
        std::env::var("ZEROG_SERVICE_TYPE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| policies::DISCOVERY_SERVICE_TYPE.to_string())
    }

    fn initialize_runtime(
        port: u16,
        display_name: Option<&str>,
        peer_id: Option<&str>,
        cert_fingerprint: Option<&str>,
    ) -> Result<DiscoveryRuntime> {
        let service_type = Self::service_type();
        let daemon = ServiceDaemon::new().context("Failed to create mDNS daemon")?;
        let _ = daemon.disable_interface(mdns_sd::IfKind::IPv6);

        let (local_service_fullname, service_info) = if port > 0 {
            let hostname = Self::local_hostname();
            let info =
                Self::build_service_info(port, display_name, peer_id, cert_fingerprint, &hostname)?;
            let fullname = info.get_fullname().to_owned();
            daemon
                .register(info.clone())
                .context("Failed to register mDNS service")?;
            tracing::debug!("mDNS service registered: {}", fullname);
            (Some(fullname), Some(info))
        } else {
            tracing::debug!("port is 0, skipping mDNS service registration");
            (None, None)
        };

        let browse_rx = match daemon.browse(&service_type) {
            Ok(browse_rx) => browse_rx,
            Err(error) => {
                if let Some(fullname) = &local_service_fullname {
                    let _ = daemon.unregister(fullname);
                }
                if let Ok(monitor) = daemon.shutdown() {
                    let _ = monitor.recv_timeout(Duration::from_millis(500));
                }
                return Err(error).context(format!("Failed to browse {service_type}"));
            }
        };

        tracing::debug!("mDNS browse started");

        Ok(DiscoveryRuntime {
            daemon,
            local_service_fullname,
            service_info,
            browse_rx,
            peers_by_service_fullname: HashMap::new(),
        })
    }

    fn background_loop(
        mut runtime: DiscoveryRuntime,
        event_tx: mpsc::Sender<DiscoveryEvent>,
        stop_rx: std_mpsc::Receiver<()>,
    ) {
        let started_at = std::time::Instant::now();
        let mut burst_index = 0;
        let mut last_announcement_heartbeat = std::time::Instant::now();

        loop {
            if stop_rx.try_recv().is_ok() {
                tracing::debug!("stop signal received, shutting down discovery");
                break;
            }

            if burst_index < policies::DISCOVERY_ANNOUNCE_BURST_DELAYS.len() {
                let target_delay = policies::DISCOVERY_ANNOUNCE_BURST_DELAYS[burst_index];
                if started_at.elapsed() >= target_delay {
                    if let Some(service_info) = &runtime.service_info {
                        if let Err(e) = runtime.daemon.register(service_info.clone()) {
                            tracing::debug!(error = %e, "failed to re-register mDNS announcement burst");
                        } else {
                            tracing::debug!(
                                burst_index,
                                elapsed_ms = started_at.elapsed().as_millis() as u64,
                                "sent mDNS announcement burst"
                            );
                        }
                    }
                    burst_index += 1;
                }
            } else if last_announcement_heartbeat.elapsed() >= policies::DISCOVERY_ANNOUNCE_HEARTBEAT_INTERVAL {
                if let Some(service_info) = &runtime.service_info {
                    if let Err(e) = runtime.daemon.register(service_info.clone()) {
                        tracing::debug!(error = %e, "failed to send mDNS announcement heartbeat");
                    } else {
                        tracing::debug!("sent mDNS announcement heartbeat");
                    }
                }
                last_announcement_heartbeat = std::time::Instant::now();
            }

            match runtime.browse_rx.recv_timeout(DISCOVERY_POLL_INTERVAL) {
                Ok(event) => {
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/zerog_debug.log") {
                        use std::io::Write;
                        let _ = writeln!(f, "[mDNS event] {:?}", event);
                    }
                    if let Some(discovery_event) = Self::discovery_event_from_service_event(
                        event,
                        runtime.local_service_fullname.as_deref(),
                        &mut runtime.peers_by_service_fullname,
                    ) {
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/zerog_debug.log") {
                            use std::io::Write;
                            let _ = writeln!(f, "[mDNS send] {:?}", discovery_event);
                        }
                        if let Err(e) = event_tx.try_send(discovery_event) {
                            if matches!(e, tokio::sync::mpsc::error::TrySendError::Closed(_)) {
                                tracing::debug!("all receivers dropped, stopping discovery");
                                break;
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    tracing::warn!("mDNS browse channel disconnected, attempting to reconnect");
                    let service_type = Self::service_type();
                    if let Ok(new_browse_rx) = runtime.daemon.browse(&service_type) {
                        runtime.browse_rx = new_browse_rx;
                    } else {
                        thread::sleep(DISCOVERY_POLL_INTERVAL);
                    }
                }
            }
        }
    }

    fn build_service_info(
        port: u16,
        display_name: Option<&str>,
        peer_id: Option<&str>,
        cert_fingerprint: Option<&str>,
        hostname: &str,
    ) -> Result<ServiceInfo> {
        let service_type = Self::service_type();

        let instance = Self::non_empty_trimmed(display_name)
            .map(Self::normalize_hostname)
            .map(|h| Self::sanitize_instance(&h))
            .unwrap_or_else(|| Self::sanitize_instance(hostname));
        let instance_name = if let Some(pid) = Self::non_empty_trimmed(peer_id) {
            let suffix = &pid[..pid.len().min(8)];
            format!("{instance}-{suffix}")
        } else {
            instance.clone()
        };
        let host_fqdn = format!("{}.local.", Self::sanitize_host(hostname));
        let mut properties = vec![
            (TXT_KEY_NAME.to_string(), instance),
            (TXT_KEY_HOSTNAME.to_string(), hostname.to_string()),
            (
                TXT_KEY_PLATFORM.to_string(),
                std::env::consts::OS.to_string(),
            ),
        ];

        if let Some(pid) = Self::non_empty_trimmed(peer_id) {
            properties.push((TXT_KEY_PEER_ID.to_string(), pid.to_string()));
        }

        if let Some(fp) = Self::non_empty_trimmed(cert_fingerprint) {
            properties.push((TXT_KEY_CERT_FINGERPRINT.to_string(), fp.to_string()));
        }

        let info = ServiceInfo::new(
            &service_type,
            &instance_name,
            &host_fqdn,
            &[] as &[&str],
            port,
            &properties[..],
        )
        .context("Failed to build mDNS service info")?;
        Ok(info.enable_addr_auto())
    }

    fn discovery_event_from_service_event(
        event: ServiceEvent,
        own_fullname: Option<&str>,
        peers_by_service_fullname: &mut HashMap<String, ResolvedServicePeer>,
    ) -> Option<DiscoveryEvent> {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let fullname = info.get_fullname().to_owned();
                if own_fullname.is_some_and(|own| own == fullname) {
                    return None;
                }
                let peer = Self::discovered_peer_from_service_info(&info)?;
                peers_by_service_fullname.insert(
                    fullname,
                    ResolvedServicePeer {
                        peer_id: peer.peer_id.clone(),
                    },
                );
                Some(DiscoveryEvent::Discovered(peer))
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                if own_fullname.is_some_and(|own| own == fullname) {
                    return None;
                }
                let peer = peers_by_service_fullname.remove(&fullname)?;
                let has_other_service = peers_by_service_fullname
                    .values()
                    .any(|p| p.peer_id == peer.peer_id);
                if has_other_service {
                    return None;
                }
                Some(DiscoveryEvent::Removed {
                    fullname,
                    peer_id: peer.peer_id,
                })
            }
            ServiceEvent::SearchStopped(_) | ServiceEvent::ServiceFound(_, _) => None,
            _ => None,
        }
    }

    #[cfg(any(target_os = "macos", test))]
    fn remove_interface_service(
        known_peers: &mut HashMap<(String, u32), String>,
        fullname: String,
        interface_index: u32,
    ) -> Option<DiscoveryEvent> {
        let peer_id = known_peers.remove(&(fullname.clone(), interface_index))?;
        if known_peers.values().any(|known| known == &peer_id) {
            return None;
        }
        Some(DiscoveryEvent::Removed { fullname, peer_id })
    }

    fn discovered_peer_from_service_info(info: &ServiceInfo) -> Option<DiscoveredPeer> {
        let (primary_addr, addrs) = Self::resolve_socket_addrs(info)?;
        let peer_id = Self::non_empty_trimmed(info.get_property_val_str(TXT_KEY_PEER_ID))
            .map(str::to_owned)?;
        let fullname = info.get_fullname().to_owned();

        let hostname = Self::normalize_hostname(
            info.get_property_val_str(TXT_KEY_HOSTNAME)
                .unwrap_or_else(|| info.get_hostname()),
        );

        let platform = Self::non_empty_trimmed(info.get_property_val_str(TXT_KEY_PLATFORM))
            .map(str::to_owned);

        Some(DiscoveredPeer {
            peer_id,
            primary_addr,
            addrs,
            hostname,
            fullname,
            platform,
        })
    }

    fn resolve_socket_addrs(info: &ServiceInfo) -> Option<(SocketAddr, Vec<SocketAddr>)> {
        let port = info.get_port();
        if port == 0 {
            return None;
        }

        let mut addrs: Vec<SocketAddr> = info
            .get_addresses()
            .iter()
            .copied()
            .filter_map(|ip| match ip {
                IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => {
                    Some(SocketAddr::new(IpAddr::V4(v4), port))
                }
                _ => None,
            })
            .collect();

        if addrs.is_empty() {
            return None;
        }

        // Sort so private LAN ranges come first, then others, link-local last, then IPv6
        addrs.sort_by_key(|sa| match sa.ip() {
            IpAddr::V4(v4) => match v4.octets() {
                [192, 168, ..] => 0,
                [10, ..] => 1,
                [172, b, ..] if (16..=31).contains(&b) => 2,
                _ if v4.is_link_local() => 4,
                _ => 3,
            },
            IpAddr::V6(_) => 5,
        });
        addrs.dedup();

        let primary_addr = addrs.first().copied()?;
        Some((primary_addr, addrs))
    }

    /// Discovers all non-loopback IPv4 addresses across network adapters.
    pub fn local_ipv4_addrs() -> Vec<Ipv4Addr> {
        let mut addrs = Vec::new();
        if let Ok(ifaces) = if_addrs::get_if_addrs() {
            for iface in ifaces {
                if !iface.is_loopback() {
                    if let if_addrs::IfAddr::V4(v4) = iface.addr {
                        if !v4.ip.is_loopback() && !v4.ip.is_link_local() && !addrs.contains(&v4.ip) {
                            addrs.push(v4.ip);
                        }
                    }
                }
            }
        }
        if addrs.is_empty() {
            if let Ok(ip) = Self::local_ipv4_fallback() {
                addrs.push(ip);
            }
        }
        addrs.sort_by_key(|ip| match ip.octets() {
            [192, 168, ..] => 0,
            [10, ..] => 1,
            [172, b, ..] if (16..=31).contains(&b) => 2,
            _ => 3,
        });
        addrs
    }

    fn local_ipv4_fallback() -> Result<Ipv4Addr> {
        let socket =
            UdpSocket::bind("0.0.0.0:0").context("Failed to create UDP socket for local IP")?;
        socket
            .connect("224.0.0.251:5353")
            .context("Failed to derive local IP via UDP connect")?;

        let ip = socket
            .local_addr()
            .context("Failed to inspect local socket address")?
            .ip();

        match ip {
            IpAddr::V4(v4) => Ok(v4),
            IpAddr::V6(_) => {
                anyhow::bail!("Unable to determine IPv4 address for mDNS advertisement")
            }
        }
    }

    fn local_hostname() -> String {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .and_then(|hostname| {
                Self::non_empty_trimmed(Some(hostname.as_str())).map(Self::normalize_hostname)
            })
            .unwrap_or_else(|| format!("mouse-thing-{}", process::id()))
    }

    fn non_empty_trimmed(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|value| !value.is_empty())
    }

    fn normalize_hostname(value: &str) -> String {
        value
            .trim_end_matches(".local")
            .trim_end_matches('.')
            .to_owned()
    }

    fn sanitize_instance(value: &str) -> String {
        let mut out = value
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect::<String>();

        if out.is_empty() {
            out = format!("mouse-thing-{}", process::id());
        }

        out
    }

    fn sanitize_host(value: &str) -> String {
        let out = value
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>();

        if out.is_empty() {
            format!("mouse-thing-{}", process::id())
        } else {
            out
        }
    }

    /// Stops the DiscoveryClient and joins the background thread.
    pub fn stop(&mut self) {
        // Send stop signal
        if let Some(ref tx) = self.stop_tx {
            let _ = tx.send(());
        }

        // Join the background thread
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }

        self.stop_tx = None;
        self.disabled_event_tx = None;
    }
}

impl Drop for DiscoveryClient {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ipv4_addrs_finds_addresses() {
        let addrs = DiscoveryClient::local_ipv4_addrs();
        assert!(!addrs.is_empty(), "Should find at least one local IPv4 address");
        for ip in addrs {
            assert!(!ip.is_loopback());
        }
    }

    #[test]
    fn test_discovered_peer_multi_address_parsing() {
        let ips = vec!["192.168.1.10", "10.0.0.5"];
        let properties = vec![
            (TXT_KEY_PEER_ID.to_string(), "peer-test-123".to_string()),
            (TXT_KEY_NAME.to_string(), "test-node".to_string()),
            (TXT_KEY_HOSTNAME.to_string(), "test-node.local".to_string()),
        ];
        let info = ServiceInfo::new(
            policies::DISCOVERY_SERVICE_TYPE,
            "test-node-123",
            "test-node.local.",
            &ips[..],
            crate::core::policies::DEFAULT_PORT,
            &properties[..],
        )
        .expect("service info should build");

        let peer = DiscoveryClient::discovered_peer_from_service_info(&info)
            .expect("should parse discovered peer");

        assert_eq!(peer.peer_id, "peer-test-123");
        assert_eq!(peer.hostname, "test-node");
        assert_eq!(peer.addrs.len(), 2);
        assert!(peer.addrs.contains(&std::net::SocketAddr::new(ips[0].parse().unwrap(), crate::core::policies::DEFAULT_PORT)));
        assert!(peer.addrs.contains(&std::net::SocketAddr::new(ips[1].parse().unwrap(), crate::core::policies::DEFAULT_PORT)));
        assert_eq!(peer.primary_addr, peer.addrs[0]);
    }

    #[test]
    fn interface_service_removal_preserves_other_interfaces_and_instances() {
        let first = "peer._zerog._udp.local.".to_string();
        let second = "peer-renamed._zerog._udp.local.".to_string();
        let mut known = HashMap::from([
            ((first.clone(), 4), "peer-a".to_string()),
            ((first.clone(), 7), "peer-a".to_string()),
            ((second.clone(), 7), "peer-a".to_string()),
        ]);

        assert!(DiscoveryClient::remove_interface_service(&mut known, first.clone(), 9).is_none());
        assert!(DiscoveryClient::remove_interface_service(&mut known, first.clone(), 4).is_none());
        assert!(DiscoveryClient::remove_interface_service(&mut known, first, 7).is_none());
        let event =
            DiscoveryClient::remove_interface_service(&mut known, second.clone(), 7).unwrap();
        assert!(
            matches!(event, DiscoveryEvent::Removed { fullname, peer_id } if fullname == second && peer_id == "peer-a")
        );
        assert!(known.is_empty());
    }

    #[test]
    fn advertisement_refreshes_interface_addresses_without_a_startup_snapshot() {
        let info = DiscoveryClient::build_service_info(
            policies::DEFAULT_PORT,
            Some("Test"),
            Some("peer-test-123"),
            None,
            "test-host",
        )
        .unwrap();

        assert!(info.is_addr_auto());
        assert!(info.get_addresses().is_empty());
        assert!(info.clone().get_addresses().is_empty());
        assert_eq!(info.get_port(), policies::DEFAULT_PORT);
        assert_eq!(
            info.get_property_val_str(TXT_KEY_PEER_ID),
            Some("peer-test-123")
        );
    }

    #[test]
    fn test_discovered_peer_filters_ipv6_addresses() {
        let ips = vec!["192.168.1.10", "fe80::c48:c5e:2dad:8e1", "fd4c:7e1c:94d2:493c:1446:9439:f95e:230"];
        let properties = vec![
            (TXT_KEY_PEER_ID.to_string(), "peer-test-456".to_string()),
            (TXT_KEY_NAME.to_string(), "test-node".to_string()),
            (TXT_KEY_HOSTNAME.to_string(), "test-node.local".to_string()),
        ];
        let info = ServiceInfo::new(
            policies::DISCOVERY_SERVICE_TYPE,
            "test-node-456",
            "test-node.local.",
            &ips[..],
            crate::core::policies::DEFAULT_PORT,
            &properties[..],
        )
        .expect("service info should build");

        let peer = DiscoveryClient::discovered_peer_from_service_info(&info)
            .expect("should parse discovered peer");

        assert_eq!(peer.addrs.len(), 1);
        assert_eq!(peer.primary_addr, std::net::SocketAddr::new(ips[0].parse().unwrap(), crate::core::policies::DEFAULT_PORT));
        assert!(peer.addrs.iter().all(|addr| addr.is_ipv4()));
    }

    #[test]
    fn test_browse_rx_disconnect_behavior() {
        let daemon = ServiceDaemon::new().unwrap();
        let service_type = policies::DISCOVERY_SERVICE_TYPE;
        let ips = vec!["192.168.1.169"];
        let properties = vec![
            (TXT_KEY_PEER_ID.to_string(), "local-peer-id".to_string()),
            (TXT_KEY_NAME.to_string(), "test-local".to_string()),
            (TXT_KEY_HOSTNAME.to_string(), "test-local".to_string()),
            (TXT_KEY_PLATFORM.to_string(), "macos".to_string()),
        ];
        let info = ServiceInfo::new(
            service_type,
            "test-local-instance",
            "test-local.local.",
            &ips[..],
            54321,
            &properties[..],
        ).unwrap();
        daemon.register(info).unwrap();
        let rx = daemon.browse(service_type).unwrap();
        for i in 0..10 {
            let res = rx.recv_timeout(Duration::from_millis(500));
            println!("event {}: {:?}", i, res);
            if matches!(res, Err(RecvTimeoutError::Disconnected)) {
                println!("DISCONNECTED at event {}", i);
                break;
            }
        }
    }
}
