use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc as std_mpsc;
use tokio::sync::mpsc;
use crate::core::discovery::{DiscoveredPeer, DiscoveryClient, DiscoveryEvent};

type DNSServiceRef = *mut c_void;
type DNSServiceFlags = u32;
type DNSServiceErrorType = i32;

const K_DNSSERVICE_ERR_NO_ERROR: DNSServiceErrorType = 0;
const K_DNSSERVICE_FLAGS_ADD: DNSServiceFlags = 0x00000002;

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 0x0001;

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: usize, timeout: c_int) -> c_int;

    fn DNSServiceBrowse(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        regtype: *const c_char,
        domain: *const c_char,
        call_back: DNSServiceBrowseReply,
        context: *mut c_void,
    ) -> DNSServiceErrorType;

    fn DNSServiceResolve(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        service_name: *const c_char,
        regtype: *const c_char,
        reply_domain: *const c_char,
        call_back: DNSServiceResolveReply,
        context: *mut c_void,
    ) -> DNSServiceErrorType;

    fn DNSServiceRegister(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        host: *const c_char,
        port: u16,
        txt_len: u16,
        txt_record: *const u8,
        call_back: Option<DNSServiceRegisterReply>,
        context: *mut c_void,
    ) -> DNSServiceErrorType;

    fn DNSServiceProcessResult(sd_ref: DNSServiceRef) -> DNSServiceErrorType;
    fn DNSServiceRefDeallocate(sd_ref: DNSServiceRef);
    fn DNSServiceRefSockFD(sd_ref: DNSServiceRef) -> c_int;
}

type DNSServiceBrowseReply = unsafe extern "C" fn(
    sd_ref: DNSServiceRef,
    flags: DNSServiceFlags,
    interface_index: u32,
    error_code: DNSServiceErrorType,
    service_name: *const c_char,
    regtype: *const c_char,
    reply_domain: *const c_char,
    context: *mut c_void,
);

type DNSServiceResolveReply = unsafe extern "C" fn(
    sd_ref: DNSServiceRef,
    flags: DNSServiceFlags,
    interface_index: u32,
    error_code: DNSServiceErrorType,
    fullname: *const c_char,
    hosttarget: *const c_char,
    port: u16,
    txt_len: u16,
    txt_record: *const u8,
    context: *mut c_void,
);

type DNSServiceRegisterReply = unsafe extern "C" fn(
    sd_ref: DNSServiceRef,
    flags: DNSServiceFlags,
    error_code: DNSServiceErrorType,
    name: *const c_char,
    regtype: *const c_char,
    domain: *const c_char,
    context: *mut c_void,
);

fn parse_txt_record(txt: &[u8]) -> HashMap<String, String> {
    let mut props = HashMap::new();
    let mut offset = 0;
    while offset < txt.len() {
        let len = txt[offset] as usize;
        offset += 1;
        if offset + len > txt.len() {
            break;
        }
        if let Ok(entry) = std::str::from_utf8(&txt[offset..offset + len]) {
            if let Some((k, v)) = entry.split_once('=') {
                props.insert(k.to_string(), v.to_string());
            }
        }
        offset += len;
    }
    props
}

fn build_txt_record(props: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (k, v) in props {
        let entry = format!("{k}={v}");
        if entry.len() <= 255 {
            bytes.push(entry.len() as u8);
            bytes.extend_from_slice(entry.as_bytes());
        }
    }
    bytes
}

struct ResolveResult {
    fullname: String,
    hosttarget: String,
    port: u16,
    txt_props: HashMap<String, String>,
}

unsafe extern "C" fn on_resolve(
    _sd_ref: DNSServiceRef,
    _flags: DNSServiceFlags,
    _interface_index: u32,
    error_code: DNSServiceErrorType,
    fullname: *const c_char,
    hosttarget: *const c_char,
    port: u16,
    txt_len: u16,
    txt_record: *const u8,
    context: *mut c_void,
) {
    if error_code != K_DNSSERVICE_ERR_NO_ERROR {
        return;
    }
    unsafe {
        let out = &mut *(context as *mut Option<ResolveResult>);
        let fn_str = CStr::from_ptr(fullname).to_string_lossy().to_string();
        let host_str = CStr::from_ptr(hosttarget).to_string_lossy().trim_end_matches('.').to_string();
        let port_host = u16::from_be(port);
        let txt_slice = if txt_len > 0 && !txt_record.is_null() {
            std::slice::from_raw_parts(txt_record, txt_len as usize)
        } else {
            &[]
        };
        let txt_props = parse_txt_record(txt_slice);
        *out = Some(ResolveResult {
            fullname: fn_str,
            hosttarget: host_str,
            port: port_host,
            txt_props,
        });
    }
}

fn resolve_service(
    interface_index: u32,
    service_name: &CStr,
    regtype: &CStr,
    reply_domain: &CStr,
) -> Option<ResolveResult> {
    let mut resolve_ref: DNSServiceRef = std::ptr::null_mut();
    let mut result: Option<ResolveResult> = None;
    unsafe {
        let err = DNSServiceResolve(
            &mut resolve_ref,
            0,
            interface_index,
            service_name.as_ptr(),
            regtype.as_ptr(),
            reply_domain.as_ptr(),
            on_resolve,
            &mut result as *mut _ as *mut c_void,
        );
        if err != K_DNSSERVICE_ERR_NO_ERROR {
            return None;
        }
        let fd = DNSServiceRefSockFD(resolve_ref);
        let mut pfd = PollFd { fd, events: POLLIN, revents: 0 };
        if poll(&mut pfd, 1, 1500) > 0 && (pfd.revents & POLLIN) != 0 {
            let _ = DNSServiceProcessResult(resolve_ref);
        }
        DNSServiceRefDeallocate(resolve_ref);
    }
    result
}

struct BrowseContext {
    event_tx: mpsc::Sender<DiscoveryEvent>,
    local_service_fullname: Option<String>,
    local_peer_id: Option<String>,
    known_peers: HashMap<(String, u32), String>,
}

unsafe extern "C" fn on_browse(
    _sd_ref: DNSServiceRef,
    flags: DNSServiceFlags,
    interface_index: u32,
    error_code: DNSServiceErrorType,
    service_name: *const c_char,
    regtype: *const c_char,
    reply_domain: *const c_char,
    context: *mut c_void,
) {
    if error_code != K_DNSSERVICE_ERR_NO_ERROR {
        return;
    }
    let ctx = unsafe { &mut *(context as *mut BrowseContext) };
    let s_name = unsafe { CStr::from_ptr(service_name) };
    let r_type = unsafe { CStr::from_ptr(regtype) };
    let r_dom = unsafe { CStr::from_ptr(reply_domain) };

    if (flags & K_DNSSERVICE_FLAGS_ADD) != 0 {
        if let Some(res) = resolve_service(interface_index, s_name, r_type, r_dom) {
            if let Some(local_fn) = &ctx.local_service_fullname {
                if res.fullname == *local_fn {
                    return;
                }
            }
            let peer_id = match res.txt_props.get("peer_id") {
                Some(id) if !id.trim().is_empty() => id.clone(),
                _ => return,
            };
            if let Some(local_id) = &ctx.local_peer_id {
                if peer_id == *local_id {
                    return;
                }
            }
            let hostname = res.txt_props.get("hostname")
                .or_else(|| res.txt_props.get("name"))
                .cloned()
                .unwrap_or_else(|| res.hosttarget.clone());
            let platform = res.txt_props.get("platform").cloned();
            let addrs_iter = (res.hosttarget.as_str(), res.port).to_socket_addrs().ok();
            let mut addrs: Vec<SocketAddr> = match addrs_iter {
                Some(it) => it.filter(|addr| addr.is_ipv4()).collect(),
                None => Vec::new(),
            };
            addrs.sort_by_key(|sa| match sa.ip() {
                std::net::IpAddr::V4(v4) => match v4.octets() {
                    [192, 168, ..] => 0,
                    [10, ..] => 1,
                    [172, b, ..] if (16..=31).contains(&b) => 2,
                    _ if v4.is_link_local() => 4,
                    _ => 3,
                },
                std::net::IpAddr::V6(_) => 5,
            });
            if let Some(&primary_addr) = addrs.first() {
                ctx.known_peers
                    .insert((res.fullname.clone(), interface_index), peer_id.clone());
                let peer = DiscoveredPeer {
                    peer_id,
                    primary_addr,
                    addrs,
                    hostname,
                    fullname: res.fullname,
                    platform,
                };
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/zerog_debug.log") {
                    use std::io::Write;
                    let _ = writeln!(f, "[macOS DNS-SD discovered] {:?}", peer);
                }
                let _ = ctx.event_tx.try_send(DiscoveryEvent::Discovered(peer));
            }
        }
    } else {
        let fn_guess = format!(
            "{}.{}.{}.",
            s_name.to_string_lossy(),
            r_type.to_string_lossy().trim_end_matches('.'),
            r_dom.to_string_lossy().trim_end_matches('.'),
        );
        if let Some(event) = DiscoveryClient::remove_interface_service(
            &mut ctx.known_peers,
            fn_guess,
            interface_index,
        ) {
            let _ = ctx.event_tx.try_send(event);
        }
    }
}

pub(super) fn run_native_discovery(
    port: u16,
    display_name: Option<&str>,
    peer_id: Option<&str>,
    cert_fingerprint: Option<&str>,
    event_tx: mpsc::Sender<DiscoveryEvent>,
    stop_rx: std_mpsc::Receiver<()>,
    startup_tx: std_mpsc::SyncSender<anyhow::Result<()>>,
) {
    let mut reg_ref: DNSServiceRef = std::ptr::null_mut();
    let mut local_fullname = None;

    if port > 0 {
        let instance = display_name.unwrap_or("ZeroG");
        let instance_name = if let Some(pid) = peer_id {
            let suffix = &pid[..pid.len().min(8)];
            format!("{instance}-{suffix}")
        } else {
            instance.to_string()
        };
        let c_instance = match CString::new(instance_name.clone()) {
            Ok(s) => s,
            Err(e) => {
                let _ = startup_tx.send(Err(anyhow::anyhow!("{e}")));
                return;
            }
        };
        let c_regtype = match CString::new("_zerog._udp.") {
            Ok(s) => s,
            Err(e) => {
                let _ = startup_tx.send(Err(anyhow::anyhow!("{e}")));
                return;
            }
        };
        let mut props = Vec::new();
        if let Some(pid) = peer_id {
            props.push(("peer_id", pid));
        }
        props.push(("name", instance));
        props.push(("hostname", instance));
        props.push(("platform", "macos"));
        if let Some(fp) = cert_fingerprint {
            props.push(("cert_fingerprint", fp));
        }
        let txt_bytes = build_txt_record(&props);

        let err = unsafe {
            DNSServiceRegister(
                &mut reg_ref,
                0,
                0,
                c_instance.as_ptr(),
                c_regtype.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                port.to_be(),
                txt_bytes.len() as u16,
                txt_bytes.as_ptr(),
                None,
                std::ptr::null_mut(),
            )
        };
        if err != K_DNSSERVICE_ERR_NO_ERROR {
            let _ = startup_tx.send(Err(anyhow::anyhow!("DNSServiceRegister failed with error {err}")));
            return;
        }
        local_fullname = Some(format!("{instance_name}._zerog._udp.local."));
    }

    let mut browse_ref: DNSServiceRef = std::ptr::null_mut();
    let c_regtype = match CString::new("_zerog._udp.") {
        Ok(s) => s,
        Err(e) => {
            unsafe {
                if !reg_ref.is_null() {
                    DNSServiceRefDeallocate(reg_ref);
                }
            }
            let _ = startup_tx.send(Err(anyhow::anyhow!("{e}")));
            return;
        }
    };
    let mut context = Box::new(BrowseContext {
        event_tx,
        local_service_fullname: local_fullname,
        local_peer_id: peer_id.map(String::from),
        known_peers: HashMap::new(),
    });

    let err = unsafe {
        DNSServiceBrowse(
            &mut browse_ref,
            0,
            0,
            c_regtype.as_ptr(),
            std::ptr::null(),
            on_browse,
            context.as_mut() as *mut _ as *mut c_void,
        )
    };
    if err != K_DNSSERVICE_ERR_NO_ERROR {
        unsafe {
            if !reg_ref.is_null() {
                DNSServiceRefDeallocate(reg_ref);
            }
        }
        let _ = startup_tx.send(Err(anyhow::anyhow!("DNSServiceBrowse failed with error {err}")));
        return;
    }

    let _ = startup_tx.send(Ok(()));

    let fd = unsafe { DNSServiceRefSockFD(browse_ref) };
    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        let mut pfd = PollFd { fd, events: POLLIN, revents: 0 };
        let ret = unsafe { poll(&mut pfd, 1, 100) };
        if ret > 0 && (pfd.revents & POLLIN) != 0 {
            let res = unsafe { DNSServiceProcessResult(browse_ref) };
            if res != K_DNSSERVICE_ERR_NO_ERROR {
                break;
            }
        }
    }

    unsafe {
        if !browse_ref.is_null() {
            DNSServiceRefDeallocate(browse_ref);
        }
        if !reg_ref.is_null() {
            DNSServiceRefDeallocate(reg_ref);
        }
    }
}
