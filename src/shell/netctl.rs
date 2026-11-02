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
    /// Syscall quirk — fail open (let sign-in proceed and fail on its own).
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

/// Pure decision: should the offline gate block sign-in? `Unknown` fails
/// open — the check must never brick sign-in on an API quirk.
pub(crate) fn blocks_sign_in(state: NetworkState) -> bool {
    state == NetworkState::Offline
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_offline_blocks_sign_in() {
        assert!(blocks_sign_in(NetworkState::Offline));
        assert!(!blocks_sign_in(NetworkState::Online));
        assert!(!blocks_sign_in(NetworkState::Unknown), "fail open");
    }
}
