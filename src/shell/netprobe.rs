//! Phase 2 of network validation (TODO 7b): a lightweight link probe that
//! turns gateway RTT (+ optional downlink) into a RECOMMENDED bitrate cap.
//! Strictly ADVISORY — the user applies it from the banner; nothing
//! auto-tunes. Blocking by design: always run on a worker thread.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Suggested-cap probe result (kbps = 0 means "no suggestion").
#[derive(Debug, Clone)]
pub(crate) struct ProbeResult {
    pub(crate) gateway: String,
    pub(crate) rtt_ms: Option<f64>,
    pub(crate) downlink_mbps: Option<f64>,
    pub(crate) suggested_kbps: u32,
}

/// Pure heuristic (unit-tested): gateway RTT + downlink → suggested cap.
///
/// v1.1 (2026-11-02, after the cap-2500 device lesson): only suggest BELOW
/// 5000 when the link is PROVABLY bad. Evidence: S5 NACK/RTX + grace make
/// 5 M viable on mediocre radio (dam 0.066/s calm day; user-preferred 5 M
/// over an advisor-suggested 2500), while under-capping costs race-start
/// latency (697 ms at 2500) and per-frame quality. RTT alone no longer
/// down-caps a healthy-looking link.
pub(crate) fn suggest_kbps(rtt_ms: Option<f64>, downlink_mbps: Option<f64>) -> u32 {
    if let Some(dl) = downlink_mbps {
        if dl < 0.8 {
            return 1_000;
        }
        if dl < 2.5 {
            return 1_500;
        }
        // 2.5–6 Mbps: 2500 ONLY when RTT also proves congestion.
        if dl < 6.0 {
            return if rtt_ms.map_or(false, |r| r >= 15.0) {
                2_500
            } else {
                5_000
            };
        }
        return 5_000;
    }
    match rtt_ms {
        Some(rtt) if rtt < 15.0 => 5_000,
        Some(rtt) if rtt < 40.0 => 1_500,
        Some(_) => 1_000,
        None => 0,
    }
}

/// Blocking probe: TCP connect RTT to the gateway (:80) + a bounded GET
/// read-back for downlink. Total budget ≈ 3 s worst case.
pub(crate) fn run_probe(gateway: String) -> ProbeResult {
    let (rtt_ms, downlink_mbps) = probe_gateway(&gateway);
    let suggested_kbps = suggest_kbps(rtt_ms, downlink_mbps);
    ProbeResult { gateway, rtt_ms, downlink_mbps, suggested_kbps }
}

fn probe_gateway(gateway: &str) -> (Option<f64>, Option<f64>) {
    let addr = format!("{gateway}:80");
    let started = Instant::now();
    let Ok(mut stream) = TcpStream::connect_timeout(
        &addr.parse().unwrap_or_else(|_| {
            // gateway IPs from sceNetCtl are dotted quads; unreachable parse
            // errors degrade to a failed probe
            "0.0.0.0:80".parse().expect("valid placeholder addr")
        }),
        Duration::from_millis(1500),
    ) else {
        return (None, None);
    };
    let rtt_ms = started.elapsed().as_secs_f64() * 1000.0;
    stream.set_read_timeout(Some(Duration::from_millis(1200))).ok();
    stream.set_write_timeout(Some(Duration::from_millis(500))).ok();

    const MIN_TRUSTWORTHY_BYTES: usize = 64 * 1024;
    if stream
        .write_all(b"GET / HTTP/1.1\r\nHost: gateway\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return (Some(rtt_ms), None);
    }
    let dl_started = Instant::now();
    let mut total = 0usize;
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break, // server closed
            Ok(n) => {
                total += n;
                if total >= MIN_TRUSTWORTHY_BYTES
                    || dl_started.elapsed() >= Duration::from_millis(1200)
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let downlink = if total >= MIN_TRUSTWORTHY_BYTES {
        let secs = dl_started.elapsed().as_secs_f64().max(0.001);
        Some(total as f64 * 8.0 / 1e6 / secs)
    } else {
        None // small redirect page / blocked port — not a bandwidth sample
    };
    (Some(rtt_ms), downlink)
}

#[cfg(test)]
mod tests {
    use super::suggest_kbps;

    #[test]
    fn calm_fast_link_suggests_5000() {
        assert_eq!(suggest_kbps(Some(1.5), Some(40.0)), 5_000);
        assert_eq!(suggest_kbps(Some(1.5), None), 5_000);
        // v1.1: mediocre RTT no longer down-caps a link with headroom
        assert_eq!(suggest_kbps(Some(10.0), None), 5_000);
    }

    #[test]
    fn provably_bad_link_down_caps() {
        assert_eq!(suggest_kbps(Some(50.0), None), 1_000);
        assert_eq!(suggest_kbps(Some(1.0), Some(1.0)), 1_500);
        assert_eq!(suggest_kbps(Some(1.0), Some(0.2)), 1_000);
    }

    #[test]
    fn mid_downlink_needs_congested_rtt_for_2500() {
        assert_eq!(suggest_kbps(Some(10.0), Some(3.0)), 5_000);
        assert_eq!(suggest_kbps(Some(30.0), Some(3.0)), 2_500);
    }

    #[test]
    fn failed_probe_suggests_nothing() {
        assert_eq!(suggest_kbps(None, None), 0);
    }
}
