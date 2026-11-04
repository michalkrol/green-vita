//! Network state via sceNetCtl — Phase 1 of network validation (TODO 7a).
//! Vita-only syscall; host builds report Online so UI flow and tests are
//! unchanged. Fail-open on unexpected syscall errors: this check is a UX
//! guard before the automatic sign-in, not a hard gate.

/// Result of the pre-sign-in connectivity check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkState {
    /// Wi-Fi connected (a routable IP address is present).
    Online,
    /// No connection — sign-in would fail; show the offline screen.
    Offline,
    /// Syscall error — treated as Offline (fail-CLOSED: the user report
    /// "crashes on turn on with Wi-Fi off" traced to this gate failing
    /// OPEN on exactly this path, letting sign-in proceed into an
    /// uninitialized network stack).
    Unknown,
}

#[cfg(target_os = "vita")]
pub(crate) fn wifi_state() -> NetworkState {
    use std::ffi::CStr;
    use vitasdk_sys::{
        sceNetCtlInetGetInfo, sceNetCtlInit, SCE_NETCTL_INFO_GET_IP_ADDRESS, SceNetCtlInfo,
    };

    unsafe {
        // sceNetCtl needs its own init refcount; the streaming stack may or
        // may not have raised it yet. Init once, never term (app lifetime).
        let _ = sceNetCtlInit();
        let mut info: SceNetCtlInfo = std::mem::zeroed();
        let result =
            sceNetCtlInetGetInfo(SCE_NETCTL_INFO_GET_IP_ADDRESS as std::ffi::c_int, &mut info);
        if result < 0 {
            return NetworkState::Unknown;
        }
        let ip = CStr::from_ptr(info.ip_address.as_ptr()).to_string_lossy();
        if ip.is_empty() || ip == "0.0.0.0" {
            NetworkState::Offline
        } else {
            NetworkState::Online
        }
    }
}

#[cfg(not(target_os = "vita"))]
pub(crate) fn wifi_state() -> NetworkState {
    NetworkState::Online
}

/// Local IP address via sceNetCtl — used by the cap-probe loopback RTT.
/// Host builds return None → loopback probe returns None.
#[cfg(target_os = "vita")]
pub(crate) fn local_ip() -> Option<String> {
    use std::ffi::CStr;
    use vitasdk_sys::{
        sceNetCtlInetGetInfo, sceNetCtlInit, SCE_NETCTL_INFO_GET_IP_ADDRESS, SceNetCtlInfo,
    };

    unsafe {
        let _ = sceNetCtlInit();
        let mut info: SceNetCtlInfo = std::mem::zeroed();
        let result =
            sceNetCtlInetGetInfo(SCE_NETCTL_INFO_GET_IP_ADDRESS as std::ffi::c_int, &mut info);
        if result < 0 {
            return None;
        }
        let ip = CStr::from_ptr(info.ip_address.as_ptr()).to_string_lossy();
        if ip.is_empty() || ip == "0.0.0.0" {
            None
        } else {
            Some(ip.to_string())
        }
    }
}

#[cfg(not(target_os = "vita"))]
pub(crate) fn local_ip() -> Option<String> {
    None
}

/// Default route (gateway IP) via sceNetCtl — the cap advisor's probe
/// target. Host builds return None (no banner).
#[cfg(target_os = "vita")]
pub(crate) fn default_route() -> Option<String> {
    use vitasdk_sys::{
        sceNetCtlInetGetInfo, sceNetCtlInit, SCE_NETCTL_INFO_GET_DEFAULT_ROUTE, SceNetCtlInfo,
    };

    unsafe {
        let _ = sceNetCtlInit();
        let mut info: SceNetCtlInfo = std::mem::zeroed();
        if sceNetCtlInetGetInfo(
            SCE_NETCTL_INFO_GET_DEFAULT_ROUTE as std::ffi::c_int,
            &mut info,
        ) < 0
        {
            return None;
        }
        let route =
            std::ffi::CStr::from_ptr(info.default_route.as_ptr()).to_string_lossy().to_string();
        (!route.is_empty()).then_some(route)
    }
}

#[cfg(not(target_os = "vita"))]
pub(crate) fn default_route() -> Option<String> {
    None
}

/// Pure decision: should the offline gate block sign-in? `Unknown` fails
/// open — the check must never brick sign-in on an API quirk.
pub(crate) fn blocks_sign_in(state: NetworkState) -> bool {
    state != NetworkState::Online
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_online_allows_sign_in() {
        assert!(blocks_sign_in(NetworkState::Offline));
        assert!(blocks_sign_in(NetworkState::Unknown), "fail CLOSED: no crashes on turn-on with Wi-Fi off");
        assert!(!blocks_sign_in(NetworkState::Online));
    }
}
