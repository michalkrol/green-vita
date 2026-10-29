//! 8b investigation (STAGE8 decision rule): RAM-buffered capture of
//! `sh_bytes_mismatch` events.
//!
//! When the shadow comparator finds two Complete AUs with identical sequence
//! bounds but different bytes, the evidence for that pair is snapshotted here:
//! RTP timestamp, bounds, packet count, the (seq, len, hash) digest list of
//! BOTH paths in arrival order, the first differing byte offset, the NAL types
//! around that offset, and both outputs' lengths and hashes.
//!
//! Constraints (per investigation plan):
//! - capped at MAX_EVENTS (16) per session so a burst cannot use up memory;
//! - RAM only during the session — written once at session end (hooked from
//!   `metrics_log::flush`, which runs from `RtcWorker::drop`), never during
//!   streaming;
//! - observe-only: no contact with pending, the submit path, or timing;
//! - allocation happens only when a mismatch actually fires (`capture`) or on
//!   the first packet of a new frame in the arrival ring (fixed-capacity
//!   arrays otherwise — zero heap traffic on the hot path).

use std::sync::Mutex;

/// Hard cap on retained snapshots per session (senior's instruction: ~16).
pub(crate) const MAX_EVENTS: usize = 16;
/// Per-frame digest capacity (arrival ring); overflow is counted, not stored.
/// 64 covers worst observed frames (~36 packets at 11.7 Mbps bursts) with
/// ~2x headroom; bigger AUs degrade to a truncated digest list.
const MAX_FRAME_PKTS: usize = 64;
/// Recent frames kept in the arrival ring (only used until both paths resolve
/// a frame — the markerless backstop is 40 ms ≈ 1-2 frames, so 4 slots is
/// generous). Kept deliberately small: the ring lives inside `VideoRtp`,
/// which is constructed as a local on the `green-vita-rtc` worker thread's
/// default-size stack (total ring ≈ 2.1 KB).
const MAX_RING_FRAMES: usize = 4;
/// NAL start codes are reported when within this many bytes of the diff.
const NAL_SCAN_WINDOW: u64 = 64;
/// Maximum NAL probes recorded per event (both sides combined).
const MAX_NAL_PROBES: usize = 8;

/// One packet's identity: sequence number, payload length, FNV-1a payload
/// hash (same `hash_bytes` as the comparator uses for whole AUs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PacketDigest {
    pub seq: u16,
    pub len: u16,
    pub hash: u64,
}

/// A NAL unit header location relevant to the mismatch: which side's output
/// (`on_new`), byte offset of the start code, and the low 5 bits of the NAL
/// header byte (the nal_unit_type).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct NalProbe {
    pub on_new: bool,
    pub offset: usize,
    pub nal_type: u8,
}

/// One captured `sh_bytes_mismatch` event. All fields owned; the store holds
/// at most MAX_EVENTS of these.
#[derive(Debug)]
pub(crate) struct MismatchSnapshot {
    pub ts: u32,
    pub first_seq: u16,
    pub last_seq: u16,
    pub packet_count: u16,
    pub legacy_digests: Box<[PacketDigest]>,
    pub new_digests: Box<[PacketDigest]>,
    pub legacy_len: usize,
    pub new_len: usize,
    pub legacy_hash: Option<u64>,
    pub new_hash: Option<u64>,
    /// First byte at which the two outputs differ, `None` only if the bytes
    /// are identical (hash collision) — reported explicitly as such.
    pub diff_offset: Option<usize>,
    pub nals: Box<[NalProbe]>,
}

static EVENTS: Mutex<Vec<MismatchSnapshot>> = Mutex::new(Vec::new());

/// Snapshot one mismatch event. Allocation only happens here, and only while
/// under the cap — a burst beyond MAX_EVENTS is counted by the CSV counter
/// but not stored.
#[allow(clippy::too_many_arguments)]
pub(crate) fn capture(
    ts: u32,
    first_seq: u16,
    last_seq: u16,
    packet_count: u16,
    legacy_digests: &[PacketDigest],
    new_digests: &[PacketDigest],
    legacy_data: &[u8],
    new_data: &[u8],
    legacy_hash: Option<u64>,
    new_hash: Option<u64>,
) {
    let mut events = EVENTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if events.len() >= MAX_EVENTS {
        return;
    }
    let diff_offset = first_diff(legacy_data, new_data);
    let nals = nal_probes(legacy_data, new_data, diff_offset);
    events.push(MismatchSnapshot {
        ts,
        first_seq,
        last_seq,
        packet_count,
        legacy_digests: legacy_digests.to_vec().into_boxed_slice(),
        new_digests: new_digests.to_vec().into_boxed_slice(),
        legacy_len: legacy_data.len(),
        new_len: new_data.len(),
        legacy_hash,
        new_hash,
        diff_offset,
        nals: nals.into_boxed_slice(),
    });
}

/// Index of the first differing byte. Equal outputs (hash collision) -> None;
/// one output being a prefix of the other -> the common length.
fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    let min = a.len().min(b.len());
    let mut i = 0;
    while i < min && a[i] == b[i] {
        i += 1;
    }
    if i == min {
        if a.len() == b.len() {
            None
        } else {
            Some(min)
        }
    } else {
        Some(i)
    }
}

/// NAL types around the mismatch: the first NAL of each side (so the list is
/// never empty for non-empty outputs) plus every start code within
/// NAL_SCAN_WINDOW of the diff offset, capped at MAX_NAL_PROBES.
fn nal_probes(legacy: &[u8], new: &[u8], offset: Option<usize>) -> Vec<NalProbe> {
    let mut out = Vec::new();
    for (data, on_new) in [(legacy, false), (new, true)] {
        let mut i = 0;
        let mut first = true;
        while i + 3 <= data.len() && out.len() < MAX_NAL_PROBES {
            let (start, hdr) = if i + 4 <= data.len() && data[i..i + 4] == [0, 0, 0, 1] {
                (i, i + 4)
            } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                (i, i + 3)
            } else {
                i += 1;
                continue;
            };
            let near = offset.is_some_and(|o| {
                (o as i64 - start as i64).unsigned_abs() <= NAL_SCAN_WINDOW
            });
            if first || near {
                if hdr < data.len() {
                    out.push(NalProbe {
                        on_new,
                        offset: start,
                        nal_type: data[hdr] & 0x1f,
                    });
                }
                first = false;
            }
            i = start + 3;
        }
    }
    out
}

/// Render every retained event (all fields) as a stable, greppable text
/// report. Returns an empty string when there are no events.
#[cfg(test)]
pub(crate) fn render() -> String {
    let events = EVENTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    render_locked(&events)
}

fn render_locked(events: &[MismatchSnapshot]) -> String {
    let mut text = String::new();
    for (index, event) in events.iter().enumerate() {
        text.push_str(&format!(
            "event={} ts={} first_seq={} last_seq={} packets={} diff_offset={}\n",
            index + 1,
            event.ts,
            event.first_seq,
            event.last_seq,
            event.packet_count,
            event
                .diff_offset
                .map_or_else(|| "none(hash-collision)".to_string(), |o| o.to_string()),
        ));
        text.push_str(&format!(
            "  legacy: len={} hash={:?} digests={}\n",
            event.legacy_len,
            event.legacy_hash,
            fmt_digests(&event.legacy_digests),
        ));
        text.push_str(&format!(
            "  new:    len={} hash={:?} digests={}\n",
            event.new_len,
            event.new_hash,
            fmt_digests(&event.new_digests),
        ));
        text.push_str("  nals:");
        for probe in event.nals.iter() {
            text.push_str(&format!(
                " {}@{}=t{}",
                if probe.on_new { "N" } else { "L" },
                probe.offset,
                probe.nal_type,
            ));
        }
        text.push('\n');
    }
    text
}

fn fmt_digests(digests: &[PacketDigest]) -> String {
    digests
        .iter()
        .map(|d| format!("{}:{:x}:{:016x}", d.seq, d.len, d.hash))
        .collect::<Vec<_>>()
        .join(",")
}

/// Fixed-capacity ring of recent frames' arrival-order packet digests.
/// Arrays only — no heap traffic on the packet hot path; `digests_for`
/// allocates once per Complete resolution (the evidence-copy the comparator
/// owns until pairing).
pub(crate) struct ArrivalRing {
    frames: [Option<ArrivalFrame>; MAX_RING_FRAMES],
    cursor: usize,
}

#[derive(Clone, Copy)]
struct ArrivalFrame {
    ts: u32,
    len: usize,
    overflow: u16,
    packets: [PacketDigest; MAX_FRAME_PKTS],
}

impl ArrivalRing {
    pub(crate) fn new() -> Self {
        Self {
            frames: [None; MAX_RING_FRAMES],
            cursor: 0,
        }
    }

    /// Record one packet of `ts` in arrival order. Duplicates are kept (the
    /// duplicate-with-altered-payload hypothesis is exactly what arrival
    /// order must expose). Overflow beyond MAX_FRAME_PKTS is counted.
    pub(crate) fn push(&mut self, ts: u32, seq: u16, payload: &[u8]) {
        let digest = PacketDigest {
            seq,
            len: payload.len() as u16,
            hash: super::hash_bytes(payload),
        };
        for slot in self.frames.iter_mut() {
            if let Some(frame) = slot {
                if frame.ts == ts {
                    if frame.len < MAX_FRAME_PKTS {
                        frame.packets[frame.len] = digest;
                        frame.len += 1;
                    } else {
                        frame.overflow = frame.overflow.saturating_add(1);
                    }
                    return;
                }
            }
        }
        let vacant = self.frames.iter().position(|slot| slot.is_none());
        let index = match vacant {
            Some(index) => index,
            None => {
                let index = self.cursor;
                self.cursor = (self.cursor + 1) % MAX_RING_FRAMES;
                index
            }
        };
        self.frames[index] = Some(ArrivalFrame {
            ts,
            len: 1,
            overflow: 0,
            packets: [PacketDigest {
                seq: 0,
                len: 0,
                hash: 0,
            }; MAX_FRAME_PKTS],
        });
        if let Some(frame) = self.frames[index].as_mut() {
            frame.packets[0] = digest;
        }
    }

    /// Owned copy of the digest list for `ts` (empty if the ring has already
    /// evicted the frame — evidence degrades gracefully, never panics).
    pub(crate) fn digests_for(&self, ts: u32) -> Box<[PacketDigest]> {
        for slot in &self.frames {
            if let Some(frame) = slot
                && frame.ts == ts
            {
                return frame.packets[..frame.len].to_vec().into_boxed_slice();
            }
        }
        Box::new([])
    }
}

/// Session-end flush: render, drain the store, and write once to
/// `ux0:data/xcloud-rust/mismatch-events.log`. Never called during streaming.
/// Tolerant when the path is unavailable (off-device builds).
pub(crate) fn session_flush() {
    let text = {
        let mut events = EVENTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if events.is_empty() {
            return;
        }
        let text = render_locked(&events);
        events.clear();
        text
    };
    match std::fs::write("ux0:data/xcloud-rust/mismatch-events.log", &text) {
        Ok(()) => eprintln!(
            "mismatch-events.log flushed ({} bytes)",
            text.len()
        ),
        Err(error) => eprintln!("mismatch events: path unavailable, skipping flush (off-device build); {error}"),
    }
}

/// Test/session helpers.
#[cfg(test)]
pub(crate) fn count() -> usize {
    EVENTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
}

#[cfg(test)]
pub(crate) fn clear() {
    EVENTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::streaming::rtc::rtp::{
        ShadowClass, ShadowComparator, ShadowRecord,
    };

    /// Serializes store-touching tests (the EVENTS map is process-global).
    fn test_gate() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    const TS: u32 = 90_000;

    fn digests(first_seq: u16, count: u16, salt: u64) -> Box<[PacketDigest]> {
        (0..count)
            .map(|offset| PacketDigest {
                seq: first_seq.wrapping_add(offset),
                len: 1400,
                hash: 0xABCD_0000_0000_0000 ^ salt ^ u64::from(offset),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    /// Annex-B test AU: SPS + IDR slice; `mutate` flips one payload byte.
    fn annex_b(mutate: bool) -> Vec<u8> {
        let mut data = vec![0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1e];
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]);
        if mutate {
            data[14] ^= 0x01; // inside the IDR NAL payload
        }
        data
    }

    fn complete_record(
        class: ShadowClass,
        digests: Box<[PacketDigest]>,
        data: Vec<u8>,
        hash: Option<u64>,
    ) -> ShadowRecord {
        ShadowRecord {
            ts: TS,
            class,
            first_seq: 100,
            last_seq: 109,
            packet_count: 10,
            bytes_hash: hash,
            digests,
            data: data.into(),
        }
    }

    #[test]
    fn forced_mismatch_produces_one_complete_snapshot() {
        let _gate = test_gate();
        clear();
        let legacy_data = annex_b(false);
        let new_data = annex_b(true);
        let legacy_hash = Some(crate::api::streaming::rtc::rtp::hash_bytes(
            &legacy_data,
        ));
        let new_hash = Some(crate::api::streaming::rtc::rtp::hash_bytes(
            &new_data,
        ));
        let legacy = complete_record(
            ShadowClass::Complete,
            digests(100, 10, 1),
            legacy_data,
            legacy_hash,
        );
        let new_rec = complete_record(
            ShadowClass::Complete,
            digests(100, 10, 1),
            new_data,
            new_hash,
        );

        let mut comparator = ShadowComparator::new();
        comparator.compare(&legacy, &new_rec);

        assert_eq!(count(), 1, "exactly one snapshot for one mismatch");
        let text = render();
        // Every required field must be populated in the report.
        assert!(text.contains(&format!("ts={TS}")), "rtp timestamp");
        assert!(text.contains("first_seq=100"), "first_seq");
        assert!(text.contains("last_seq=109"), "last_seq");
        assert!(text.contains("packets=10"), "packet count");
        assert!(text.contains("diff_offset=14"), "first differing byte");
        assert!(
            text.contains(&format!("legacy: len={}", legacy_len())) ,
            "legacy length"
        );
        assert!(text.contains("hash=Some("), "both hashes");
        // Both digest lists present, 10 entries each, arrival order intact.
        let legacy_line = text
            .lines()
            .find(|line| line.starts_with("  legacy:"))
            .expect("legacy digest line");
        assert_eq!(legacy_line.matches(',').count(), 9, "10 entries legacy");
        let new_line = text
            .lines()
            .find(|line| line.starts_with("  new:"))
            .expect("new digest line");
        assert_eq!(new_line.matches(',').count(), 9, "10 entries new");
        assert!(legacy_line.contains("100:578:"), "seq 100 decimal, len 1400 (0x578)");
        // NAL types around the offset (SPS=7, IDR=5).
        assert!(text.contains("nals:"), "nal line present");
        assert!(text.contains("=t7") && text.contains("=t5"), "nal types");
        clear();
    }

    fn legacy_len() -> usize {
        annex_b(false).len()
    }

    #[test]
    fn cap_holds_at_max_events() {
        let _gate = test_gate();
        clear();
        for index in 0..(MAX_EVENTS + 4) {
            capture(
                1_000 + index as u32,
                1,
                2,
                2,
                &[],
                &[],
                b"aaaa",
                b"aaab",
                Some(1),
                Some(2),
            );
        }
        assert_eq!(count(), MAX_EVENTS, "store capped at MAX_EVENTS");
        clear();
    }

    #[test]
    fn no_snapshot_when_pairs_match() {
        let _gate = test_gate();
        clear();
        let data = annex_b(false);
        let hash = Some(crate::api::streaming::rtc::rtp::hash_bytes(&data));
        let legacy = complete_record(
            ShadowClass::Complete,
            digests(100, 10, 1),
            data.clone(),
            hash,
        );
        let new_rec = complete_record(
            ShadowClass::Complete,
            digests(100, 10, 1),
            data,
            hash,
        );
        let mut comparator = ShadowComparator::new();
        comparator.compare(&legacy, &new_rec);
        assert_eq!(count(), 0, "identical pair captures nothing");
        assert_eq!(render(), "", "empty render");
        clear();
    }

    #[test]
    fn session_flush_drains_the_store() {
        let _gate = test_gate();
        clear();
        for index in 0..3 {
            capture(
                2_000 + index,
                7,
                8,
                2,
                &[],
                &[],
                b"xxxx",
                b"xxyx",
                Some(3),
                Some(4),
            );
        }
        assert_eq!(count(), 3);
        session_flush(); // off-device: write is skipped, drain still happens
        assert_eq!(count(), 0, "flush drains the store");
        clear();
    }
}
