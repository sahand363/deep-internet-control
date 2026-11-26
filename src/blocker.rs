//! Per-app blocking engine (v1.0).
//!
//! v0.3–v0.9 blocked by holding EVERY packet of the system in one
//! user-space thread (WinDivert intercept) and simply not re-sending
//! the blocked app's packets. That works, but it puts the whole
//! internet behind this process: a slow tick or a hang slows or
//! freezes all traffic, and every download pays a user-space round
//! trip per packet.
//!
//! v1.0 splits the job so nothing normal ever passes through us:
//!
//! 1. **New connections** — Windows Filtering Platform filters on the
//!    ALE connect / accept layers (IPv4 + IPv6), matched on the app's
//!    id (its exe path). Opened in a *dynamic* WFP session: Windows
//!    deletes every filter by itself the moment this process exits or
//!    crashes, so nothing persists — the same promise as before.
//! 2. **Connections that were already open** — open TCP connections
//!    are terminated at once (`SetTcpEntry`, `crate::connections`,
//!    unchanged since v0.5). For whatever is still open (UDP sockets,
//!    IPv6 TCP), a WinDivert handle with the DROP flag and a narrow
//!    filter on those sockets' local ports is opened: the driver
//!    discards matching packets in the kernel, nothing is copied to
//!    user space. The filter is rebuilt ~1/s from the connection
//!    tables while any app is blocked.
//!
//! The pure parts (port-set filter building) are unit-tested on any
//! OS; the Win32 parts only build on Windows.

#[cfg(windows)]
use std::collections::HashSet;
#[cfg(windows)]
use std::sync::{Arc, RwLock};

/// Most local ports the drop filter lists; beyond this the filter
/// would get too long for WinDivert's compiler. Blocked apps rarely
/// hold more than a handful of old sockets, and new ones are caught
/// by WFP anyway.
pub const MAX_DROP_PORTS: usize = 120;

/// Build the WinDivert filter that drops the given sockets (TCP and
/// UDP local ports). `None` when there is nothing to drop. Ports are
/// sorted and de-duplicated so an unchanged set yields the same
/// string (the caller only reopens the handle when it changes).
pub fn drop_filter(tcp_ports: &[u16], udp_ports: &[u16]) -> Option<String> {
    let mut tcp: Vec<u16> = tcp_ports.iter().copied().filter(|p| *p != 0).collect();
    let mut udp: Vec<u16> = udp_ports.iter().copied().filter(|p| *p != 0).collect();
    tcp.sort_unstable();
    tcp.dedup();
    udp.sort_unstable();
    udp.dedup();
    let total = tcp.len() + udp.len();
    if total == 0 {
        return None;
    }
    // Keep within the cap, TCP first (UDP sockets are usually few).
    if tcp.len() > MAX_DROP_PORTS {
        tcp.truncate(MAX_DROP_PORTS);
    }
    let room = MAX_DROP_PORTS.saturating_sub(tcp.len());
    udp.truncate(room);
    let part = |proto: &str, ports: &[u16]| -> Option<String> {
        if ports.is_empty() {
            return None;
        }
        let ors: Vec<String> = ports.iter().map(|p| format!("localPort == {p}")).collect();
        Some(format!("({proto} and ({}))", ors.join(" or ")))
    };
    let parts: Vec<String> = [part("tcp", &tcp), part("udp", &udp)]
        .into_iter()
        .flatten()
        .collect();
    Some(format!("!loopback and ({})", parts.join(" or ")))
}

/// Start the blocking engine. Returns `true` when blocking works
/// (the WFP engine opened — needs administrator rights). The engine
/// runs in its own thread for the life of the process and follows
/// the shared blocked set.
#[cfg(windows)]
pub fn spawn(blocked: Arc<RwLock<HashSet<String>>>) -> bool {
    match win::Wfp::open() {
        Ok(wfp) => {
            crate::log::log("blocker: WFP dynamic session opened");
            std::thread::spawn(move || win::run(wfp, blocked));
            true
        }
        Err(code) => {
            crate::log::log(&format!(
                "blocker: could not open the WFP engine (error {code}) - blocking unavailable"
            ));
            false
        }
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    use windows::core::{GUID, PCWSTR, PWSTR};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::NetworkManagement::WindowsFilteringPlatform::*;

    const RPC_C_AUTHN_WINNT: u32 = 10;

    /// The open WFP engine handle. Only ever used from the blocker
    /// thread; `Send` so it can be moved there.
    pub struct Wfp {
        engine: HANDLE,
    }
    unsafe impl Send for Wfp {}

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    impl Wfp {
        pub fn open() -> Result<Self, u32> {
            let mut name = wide("Internet Control");
            let session = FWPM_SESSION0 {
                flags: FWPM_SESSION_FLAG_DYNAMIC,
                displayData: FWPM_DISPLAY_DATA0 {
                    name: PWSTR(name.as_mut_ptr()),
                    description: PWSTR::null(),
                },
                ..Default::default()
            };
            let mut engine = HANDLE::default();
            let rc = unsafe {
                FwpmEngineOpen0(PCWSTR::null(), RPC_C_AUTHN_WINNT, None, Some(&session), &mut engine)
            };
            if rc != 0 {
                return Err(rc);
            }
            Ok(Self { engine })
        }

        /// Add the four block filters for one exe. Returns their ids
        /// (to delete them on Unblock), or the WFP error code.
        pub fn block_exe(&self, exe_path: &str) -> Result<Vec<u64>, u32> {
            let path_w = wide(exe_path);
            let mut app_id: *mut FWP_BYTE_BLOB = std::ptr::null_mut();
            let rc = unsafe { FwpmGetAppIdFromFileName0(PCWSTR(path_w.as_ptr()), &mut app_id) };
            if rc != 0 || app_id.is_null() {
                return Err(rc);
            }
            let mut ids = Vec::new();
            let mut name = wide("Internet Control block");
            let layers: [GUID; 4] = [
                FWPM_LAYER_ALE_AUTH_CONNECT_V4,
                FWPM_LAYER_ALE_AUTH_CONNECT_V6,
                FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4,
                FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6,
            ];
            let mut result = Ok(());
            for layer in layers {
                let mut cond = FWPM_FILTER_CONDITION0 {
                    fieldKey: FWPM_CONDITION_ALE_APP_ID,
                    matchType: FWP_MATCH_EQUAL,
                    conditionValue: FWP_CONDITION_VALUE0 {
                        r#type: FWP_BYTE_BLOB_TYPE,
                        Anonymous: FWP_CONDITION_VALUE0_0 { byteBlob: app_id },
                    },
                };
                let mut filter = FWPM_FILTER0 {
                    displayData: FWPM_DISPLAY_DATA0 {
                        name: PWSTR(name.as_mut_ptr()),
                        description: PWSTR::null(),
                    },
                    // Hard block: a permit in another sublayer (e.g.
                    // a firewall allow rule) cannot override it.
                    flags: FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT,
                    layerKey: layer,
                    weight: FWP_VALUE0 {
                        r#type: FWP_UINT8,
                        Anonymous: FWP_VALUE0_0 { uint8: 15 },
                    },
                    numFilterConditions: 1,
                    filterCondition: &mut cond,
                    action: FWPM_ACTION0 {
                        r#type: FWP_ACTION_BLOCK,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut id: u64 = 0;
                let rc = unsafe { FwpmFilterAdd0(self.engine, &mut filter, None, Some(&mut id)) };
                if rc != 0 {
                    result = Err(rc);
                    break;
                }
                ids.push(id);
            }
            unsafe {
                let mut p = app_id as *mut core::ffi::c_void;
                FwpmFreeMemory0(&mut p);
            }
            match result {
                Ok(()) => Ok(ids),
                Err(rc) => {
                    self.unblock(&ids);
                    Err(rc)
                }
            }
        }

        pub fn unblock(&self, ids: &[u64]) {
            for id in ids {
                unsafe {
                    let _ = FwpmFilterDeleteById0(self.engine, *id);
                }
            }
        }
    }

    impl Drop for Wfp {
        fn drop(&mut self) {
            unsafe {
                let _ = FwpmEngineClose0(self.engine);
            }
        }
    }

    type DropHandle = windivert::WinDivert<windivert::layer::NetworkLayer>;

    fn close_drop(handle: &mut Option<DropHandle>) {
        if let Some(mut h) = handle.take() {
            let _ = h.close(windivert::CloseAction::Nothing);
        }
    }

    /// Local ports of every socket the blocked apps hold right now.
    fn blocked_ports(blocked: &HashSet<String>) -> (Vec<u16>, Vec<u16>) {
        let tables = crate::connections::snapshot_tables();
        let mut exe_of: HashMap<u32, Option<String>> = HashMap::new();
        let mut is_blocked = |pid: u32| -> bool {
            exe_of
                .entry(pid)
                .or_insert_with(|| crate::capture::exe_for_pid(pid).map(|p| p.to_lowercase()))
                .as_ref()
                .map(|p| blocked.contains(p))
                .unwrap_or(false)
        };
        let mut tcp = Vec::new();
        for (key, pid) in &tables.tcp {
            if is_blocked(*pid) {
                tcp.push(key.local_port);
            }
        }
        let mut udp = Vec::new();
        for ((_, port), pid) in &tables.udp {
            if is_blocked(*pid) {
                udp.push(*port);
            }
        }
        (tcp, udp)
    }

    pub fn run(wfp: Wfp, blocked: Arc<RwLock<HashSet<String>>>) {
        use windivert::prelude::*;
        let mut applied: HashMap<String, Vec<u64>> = HashMap::new();
        let mut failed: HashMap<String, Instant> = HashMap::new();
        let mut drop_handle: Option<DropHandle> = None;
        let mut drop_filter_now: Option<String> = None;
        let mut last_ports = Instant::now() - Duration::from_secs(10);
        loop {
            let want: HashSet<String> = blocked.read().map(|s| s.clone()).unwrap_or_default();
            // Unblocked apps: delete their filters.
            let gone: Vec<String> = applied.keys().filter(|k| !want.contains(*k)).cloned().collect();
            for exe in gone {
                if let Some(ids) = applied.remove(&exe) {
                    wfp.unblock(&ids);
                    crate::log::log(&format!("blocker: unblocked {exe}"));
                }
            }
            failed.retain(|k, _| want.contains(k));
            // Newly blocked apps: add filters (retry failures every 10 s,
            // e.g. an exe path that does not exist yet).
            let mut changed = false;
            for exe in &want {
                if applied.contains_key(exe) {
                    continue;
                }
                if let Some(t) = failed.get(exe) {
                    if t.elapsed() < Duration::from_secs(10) {
                        continue;
                    }
                }
                match wfp.block_exe(exe) {
                    Ok(ids) => {
                        crate::log::log(&format!("blocker: WFP filters added for {exe}"));
                        applied.insert(exe.clone(), ids);
                        failed.remove(exe);
                        changed = true;
                    }
                    Err(rc) => {
                        crate::log::log(&format!("blocker: WFP filter for {exe} failed (error {rc})"));
                        failed.insert(exe.clone(), Instant::now());
                    }
                }
            }
            // Already-open sockets of blocked apps: kernel drop handle.
            if want.is_empty() {
                if drop_handle.is_some() {
                    close_drop(&mut drop_handle);
                    drop_filter_now = None;
                }
            } else if changed || last_ports.elapsed() >= Duration::from_secs(1) {
                last_ports = Instant::now();
                let (tcp, udp) = blocked_ports(&want);
                let filter = drop_filter(&tcp, &udp);
                if filter != drop_filter_now {
                    // Open the new handle first, then close the old
                    // one, so there is no gap.
                    let new_handle = match &filter {
                        Some(f) => {
                            let flags = WinDivertFlags::default().set_drop();
                            match WinDivert::network(f, 1000, flags) {
                                Ok(h) => Some(h),
                                Err(e) => {
                                    crate::log::log(&format!("blocker: drop handle failed: {e}"));
                                    None
                                }
                            }
                        }
                        None => None,
                    };
                    close_drop(&mut drop_handle);
                    drop_handle = new_handle;
                    drop_filter_now = filter;
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sets_build_no_filter() {
        assert_eq!(drop_filter(&[], &[]), None);
        assert_eq!(drop_filter(&[0], &[0]), None);
    }

    #[test]
    fn filter_lists_tcp_and_udp_ports_sorted_and_deduped() {
        let f = drop_filter(&[443, 80, 443], &[5353]).unwrap();
        assert_eq!(
            f,
            "!loopback and ((tcp and (localPort == 80 or localPort == 443)) or (udp and (localPort == 5353)))"
        );
    }

    #[test]
    fn filter_is_stable_for_the_same_set() {
        assert_eq!(drop_filter(&[2, 1], &[]), drop_filter(&[1, 2, 2], &[]));
    }

    #[test]
    fn filter_is_capped() {
        let tcp: Vec<u16> = (1..=500).collect();
        let f = drop_filter(&tcp, &[9]).unwrap();
        assert_eq!(f.matches("localPort").count(), MAX_DROP_PORTS);
    }
}
