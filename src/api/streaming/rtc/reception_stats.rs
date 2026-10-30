//! Receiver-side RTP reception statistics for RTCP Receiver Reports
//! (RFC 3550 §6.4.2, algorithm per Appendix A.3). Pure: feed packets in,
//! build `ReceptionReport` blocks out — no I/O, host-testable.
//! Arrival times are µs on the SHADOW_CLOCK domain; RTP timestamps are the
//! 90 kHz video clock. Written for ONE video stream (first packet binds the
//! SSRC — the console's video ssrc is stable for the session).

use rtcp::reception_report::ReceptionReport;

pub(crate) struct ReceptionStats {
    ssrc: u32,
    base_seq: u16,
    max_seq: u16,
    /// Completed u16 wraps, in steps of 0x10000; cycles + max_seq = extended seq.
    cycles: u32,
    received: u64,
    /// Totals as of the previous build_report() (A.3 expected/received_prior).
    expected_prior: u64,
    received_prior: u64,
    /// Interarrival jitter in RTP timestamp units (90 kHz), RFC A.3.
    jitter: f64,
    last_arrival_us: i64,
    last_rtp_ts: u32,
    started: bool,
}

impl ReceptionStats {
    pub(crate) fn new() -> Self {
        Self {
            ssrc: 0,
            base_seq: 0,
            max_seq: 0,
            cycles: 0,
            received: 0,
            expected_prior: 0,
            received_prior: 0,
            jitter: 0.0,
            last_arrival_us: 0,
            last_rtp_ts: 0,
            started: false,
        }
    }

    pub(crate) fn on_packet(&mut self, ssrc: u32, seq: u16, rtp_ts: u32, arrival_us: i64) {
        if !self.started {
            *self = Self {
                ssrc,
                base_seq: seq,
                max_seq: seq,
                cycles: 0,
                received: 1,
                expected_prior: 1,
                received_prior: 1,
                jitter: 0.0,
                last_arrival_us: arrival_us,
                last_rtp_ts: rtp_ts,
                started: true,
            };
            // A.3: no jitter sample on the first packet.
            return;
        }
        // Newer-than-max with wrap detection: positive i16 delta means "newer"
        // across the u16 boundary; a raw seq < max_seq in that case = a wrap.
        if (seq.wrapping_sub(self.max_seq) as i16) > 0 {
            if seq < self.max_seq {
                self.cycles += 0x1_0000;
            }
            self.max_seq = seq;
        }
        self.received += 1;
        // J += (|D| − J)/16 with D = Δarrival − Δrtp(in µs). Incremental form
        // avoids the u32 RTP-timestamp wrap entirely (Δ uses wrapping_sub).
        let d_us = (arrival_us - self.last_arrival_us) as f64
            - f64::from(rtp_ts.wrapping_sub(self.last_rtp_ts) as i32) * (100.0 / 9.0);
        self.jitter += (d_us.abs() - self.jitter) / 16.0;
        self.last_arrival_us = arrival_us;
        self.last_rtp_ts = rtp_ts;
        // ssrc beyond the first packet is ignored (stable for the session).
        let _ = ssrc;
    }

    /// Build this interval's report block (fraction lost is since the PREVIOUS
    /// call, per A.3; cumulative lost and extended sequence are session-total).
    /// Returns None before the first packet.
    pub(crate) fn build_report(&mut self) -> Option<ReceptionReport> {
        if !self.started {
            return None;
        }
        let extended_max = self.cycles + u32::from(self.max_seq);
        let expected_total =
            i64::from(extended_max) - i64::from(self.base_seq) + 1;
        let total_lost = (expected_total - self.received as i64).clamp(0, 0x00FF_FFFF);
        let expected_interval =
            (expected_total.max(0) as u64).saturating_sub(self.expected_prior);
        let received_interval = self.received.saturating_sub(self.received_prior);
        let lost_interval = expected_interval as i64 - received_interval as i64;
        let fraction = if lost_interval > 0 && expected_interval > 0 {
            (((lost_interval << 8) / expected_interval as i64) as u32).min(255)
        } else {
            0
        };
        self.expected_prior = expected_total.max(0) as u64;
        self.received_prior = self.received;
        Some(ReceptionReport {
            ssrc: self.ssrc,
            fraction_lost: fraction as u8,
            total_lost: total_lost as u32,
            last_sequence_number: extended_max,
            jitter: self.jitter.round().max(0.0) as u32,
            // LSR/DLSR = 0: valid per RFC while no SR has been received.
            // Filled once an inbound-SR reader lands (RR-first probe surfaces
            // SR arrival via rtc stats first).
            last_sender_report: 0,
            delay: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_returns_none() {
        assert!(ReceptionStats::new().build_report().is_none());
    }

    #[test]
    fn steady_stream_reports_clean() {
        let mut s = ReceptionStats::new();
        for i in 0u16..10 {
            // 900 ticks (10 ms) of RTP clock; arrivals paced identically (10 ms)
            // → D = 0 each step → jitter stays ~0
            s.on_packet(0xDEAD, 100 + i, 900 * u32::from(i), 1_000_000 + i64::from(i) * 10_000);
        }
        let r = s.build_report().unwrap();
        assert_eq!(r.ssrc, 0xDEAD);
        assert_eq!(r.fraction_lost, 0);
        assert_eq!(r.total_lost, 0);
        assert_eq!(r.last_sequence_number, 109);
        assert_eq!(r.last_sender_report, 0);
        assert_eq!(r.delay, 0);
        // steady arrivals paced exactly at 90 kHz → tiny jitter
        assert!(r.jitter < 10, "jitter {}", r.jitter);
    }

    #[test]
    fn loss_shows_in_fraction_and_total() {
        let mut s = ReceptionStats::new();
        s.on_packet(1, 0, 0, 1_000);
        // seqs 1..=9 all lost, seq 10 arrives
        s.on_packet(1, 10, 90_000, 1_000 + 100 * 1_000);
        let r = s.build_report().unwrap();
        assert_eq!(r.total_lost, 9);
        // interval: expected_total 11 − prior 1 = 10; received 2 − 1 = 1;
        // lost_interval 9 → fraction = 9<<8 / 10 = 230 (A.3)
        assert_eq!(r.fraction_lost, 230);
        // second report over nothing-new resets to zeros but keeps totals
        let r2 = s.build_report().unwrap();
        assert_eq!(r2.fraction_lost, 0);
        assert_eq!(r2.total_lost, 9);
    }

    #[test]
    fn wrap_extends_sequence() {
        let mut s = ReceptionStats::new();
        // full contiguous range across the u16 boundary: 0xFFF0..=0xFFFF + 0x0000..=0x0005
        for seq in 0xFFF0..=0xFFFFu16 {
            s.on_packet(7, seq, 0, 1_000);
        }
        for seq in 0..=5u16 {
            s.on_packet(7, seq, 0, 1_000);
        }
        let r = s.build_report().unwrap();
        assert_eq!(r.last_sequence_number, 0x1_0005);
        assert_eq!(r.total_lost, 0);
        assert_eq!(r.fraction_lost, 0);
    }
}
