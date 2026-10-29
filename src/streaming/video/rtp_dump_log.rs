/// RAM-buffered RTP packet dump, flushed once at session end.
///
/// Binary format (little-endian):
/// - 8-byte magic prefix: `RTPDUMP1`
/// - Concatenated records, each:
///   [t_us: u64][seq: u16][ts: u32][marker: u8][len: u16][payload: len bytes]
///
/// Recording is gated on:
///   - `RTP_DUMP` flag (compile-time constant; flip to `false` after fixture capture)
///   - First 20 s only: `t_us < 20_000_000`
///   - 16 MiB total byte budget cap — stop recording if exceeded; never OOM
///
/// Usage:
///   - `init_rtp_dump()` is called once from `RtcSession::new`.
///   - `record()` is called from the RTP receive hot path (see `rtp.rs`).
///   - `flush_rtp_dump()` is called once at session end from `RtcWorker::drop`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

/// A single RTP dump record.
/// Record layout (little-endian, binary file format):
///   [t_us: u64][seq: u16][ts: u32][marker: u8][len: u16][payload: len bytes]
#[derive(Debug, Clone)]
pub(crate) struct Record {
    /// Monotonic µs since session start when this packet arrived.
    pub t_us: u64,
    /// RTP sequence number of this packet.
    pub seq: u16,
    /// RTP timestamp of this packet.
    pub ts: u32,
    /// Marker bit (true/false → 1/0 in the binary record).
    pub marker: bool,
    /// Payload bytes of this packet.
    pub payload: Vec<u8>,
}

/// RAM-buffered RTP packet dump log.
/// Records are accumulated in RAM and flushed once at session end to ux0:data/xcloud-rust/rtp-dump.bin.
#[derive(Debug)]
pub(crate) struct RtpDumpLog {
    /// Protected buffer of recorded records.
    records: Mutex<Vec<Record>>,
    /// Counter for total bytes recorded (for budget cap).
    total_bytes: std::sync::atomic::AtomicU64,
    /// Whether the dump has stopped (due to time/budget exceeded).
    /// Checked without taking the records mutex in the hot path.
    stopped: std::sync::atomic::AtomicBool,
}

impl RtpDumpLog {
    /// Creates a new RAM-buffered RTP dump log.
    pub(crate) fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            total_bytes: std::sync::atomic::AtomicU64::new(0),
            stopped: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Record an RTP packet if within budget and time window.
    /// Returns `true` if the record was added, `false` if gated out.
    /// This is cheap when disabled: the `RTP_DUMP` const false branch collapses
    /// away, and the `stopped` atomic is checked without lock acquisition.
    pub(crate) fn record_internal(&self, t_us: u64, seq: u16, ts: u32, marker: bool, payload: &[u8]) -> bool {
        // Fast path: check stopped flag without lock (one relaxed atomic load).
        if self.stopped.load(Ordering::Relaxed) {
            return false;
        }
        // Check time budget without lock.
        if t_us >= MAX_DURATION_US {
            self.stopped.store(true, Ordering::Relaxed);
            return false;
        }
        // Check byte budget without lock.
        let current_bytes = self.total_bytes.load(Ordering::Relaxed);
        let record_size = (RECORD_OVERHEAD + payload.len()) as u64;
        if current_bytes + record_size > MAX_TOTAL_BYTES {
            self.stopped.store(true, Ordering::Relaxed);
            return false;
        }
        // Slow path: acquire lock and add record.
        let mut guard = self.records.lock().unwrap_or_else(|_| {
            panic!("rtp_dump_log records mutex poisoned")
        });
        // Re-check stopped after acquiring lock (another thread may have set it).
        if self.stopped.load(Ordering::Relaxed) {
            return false;
        }
        // Re-check byte budget after lock (another thread may have added records).
        let guard_bytes = self.total_bytes.load(Ordering::Relaxed);
        if guard_bytes + record_size > MAX_TOTAL_BYTES {
            return false;
        }
        // Add the record.
        guard.push(Record {
            t_us,
            seq,
            ts,
            marker,
            payload: payload.to_vec(),
        });
        // Update total bytes atomically.
        self.total_bytes.store(guard_bytes + record_size, Ordering::Relaxed);
        true
    }

    /// Flush the entire buffer to ux0:data/xcloud-rust/rtp-dump.bin.
    /// Must be called once at session end (from RtcWorker::drop).
    /// Tolerant of path being unavailable (off-device: silently skip).
    pub(crate) fn flush(&self) {
        // Mark as stopped so no more records can be added.
        self.stopped.store(true, Ordering::Relaxed);

        let mut guard = self.records.lock().unwrap_or_else(|_| {
            panic!("rtp_dump_log records mutex poisoned")
        });
        let records = std::mem::take(&mut *guard);

        let path = std::path::Path::new("ux0:data/xcloud-rust/rtp-dump.bin");
        // Ensure directory exists (no-op if already exists on-device; off-device will fail).
        let _ = std::fs::create_dir_all("ux0:data/xcloud-rust");

        // Build binary content: 8-byte magic prefix "RTPDUMP1" + concatenated records.
        let mut content = Vec::new();
        // Write magic prefix "RTPDUMP1".
        content.extend_from_slice(MAGIC_PREFIX);
        // Write each record (little-endian).
        for record in &records {
            // t_us: u64 LE.
            content.extend_from_slice(&record.t_us.to_le_bytes());
            // seq: u16 LE.
            content.extend_from_slice(&record.seq.to_le_bytes());
            // ts: u32 LE.
            content.extend_from_slice(&record.ts.to_le_bytes());
            // marker: u8 (0 or 1).
            content.push(if record.marker { 1 } else { 0 });
            // len: u16 LE — payload length.
            let payload_len = record.payload.len() as u16;
            content.extend_from_slice(&payload_len.to_le_bytes());
            // payload: len bytes.
            content.extend_from_slice(&record.payload);
        }

        // Try writing the full content.
        match std::fs::write(path, &content) {
            Ok(()) => {
                eprintln!("RTP dump flushed: {} records", records.len());
            }
            Err(e) => {
                eprintln!("RTP dump: path unavailable, skipping flush (off-device build); error: {e}");
            }
        }
    }
}

//
// Module-level constants
//

/// Enable/disable RTP dump capture for the 8a device gate.
/// Flip to `false` after the fixture capture is pulled.
pub(crate) const RTP_DUMP: bool = true;

/// Magic+version prefix for the binary dump file: 8 bytes `RTPDUMP1`.
const MAGIC_PREFIX: &[u8; 8] = b"RTPDUMP1";

/// Maximum recording duration in microseconds (20 seconds).
const MAX_DURATION_US: u64 = 20_000_000;

/// Maximum total bytes to record (16 MiB).
const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;

/// Overhead bytes per record (t_us:u64 + seq:u16 + ts:u32 + marker:u8 + len:u16), no payload.
const RECORD_OVERHEAD: usize = 8 + 2 + 4 + 1 + 2; // = 17

/// Global RTP dump log for the current session.
/// Owned by the video module; flushed from RtcWorker::drop.
pub(crate) static RTP_DUMP_LOG: LazyLock<Mutex<Option<RtpDumpLog>>> =
    LazyLock::new(|| Mutex::new(None));

/// Fast-path gate: true between `init_rtp_dump()` and `flush_rtp_dump()`.
/// Lets `record()` cost a single relaxed atomic load when no session is
/// active (pre-init / post-flush), without touching the mutex.
static DUMP_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Initialize the RAM-buffered RTP dump log for a new streaming session.
/// Sets the global static to a freshly opened log (header-on-open semantics).
/// Called once from RtcSession::new so the first dump rows cannot report
/// the previous session's values.
pub(crate) fn init_rtp_dump() {
    // Flush any previous session's log that never got flushed, then replace with a fresh log.
    if let Some(old) = RTP_DUMP_LOG.lock().unwrap().take() {
        old.flush();
    }
    *RTP_DUMP_LOG.lock().unwrap() = Some(RtpDumpLog::new());
    DUMP_ACTIVE.store(true, Ordering::Release);
}

/// Flush the RAM-buffered RTP dump log to disk once at session end.
/// Called from RtcWorker::drop after session teardown; no filesystem I/O during session.
/// Sets the global log to None after flush so a new session starts clean.
pub(crate) fn flush_rtp_dump() {
    DUMP_ACTIVE.store(false, Ordering::Release);
    let mut log = RTP_DUMP_LOG.lock().unwrap();
    if let Some(log) = log.as_ref() {
        log.flush();
    }
    *log = None;
}

/// Record an RTP packet into the dump log.
/// Returns `true` if the record was added, `false` if gated out.
/// This is cheap when disabled: the `RTP_DUMP` const false branch collapses
/// away entirely at compile time. The caller must ensure the log has been
/// initialized (e.g. via `init_rtp_dump()` from `RtcSession::new`).
///
/// Arguments:
/// - `t_us`: monotonic µs since session start
/// - `seq`: RTP sequence number
/// - `ts`: RTP timestamp
/// - `marker`: marker bit
/// - `payload`: packet payload bytes (borrowed, may be empty)
pub(crate) fn record(t_us: u64, seq: u16, ts: u32, marker: bool, payload: &[u8]) -> bool {
    // Fast path when disabled — const false branch collapses entirely at compile time.
    if !RTP_DUMP {
        return false;
    }
    // Fast path when no session is active: one relaxed atomic load, no lock.
    if !DUMP_ACTIVE.load(Ordering::Relaxed) {
        return false;
    }
    let mut guard = RTP_DUMP_LOG.lock().unwrap();
    let Some(log) = guard.as_mut() else {
        return false;
    };
    log.record_internal(t_us, seq, ts, marker, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cargo runs tests in parallel threads; the process-global
    /// `RTP_DUMP_LOG` is a session singleton, so tests that touch it must
    /// serialize here (the lock also absorbs cross-test poisoning).
    static GLOBAL_LOG_LOCK: Mutex<()> = Mutex::new(());

    fn lock_globals() -> std::sync::MutexGuard<'static, ()> {
        GLOBAL_LOG_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn test_binary_format_magic_prefix() {
        // Verify the magic prefix is exactly 8 bytes "RTPDUMP1".
        let expected = b"RTPDUMP1";
        assert_eq!(MAGIC_PREFIX, expected, "Magic prefix must be 'RTPDUMP1'");
        assert_eq!(MAGIC_PREFIX.len(), 8, "Magic prefix must be 8 bytes");
    }

    #[test]
    fn test_record_round_trip() {
        // Create a record with a payload that exercises all byte values.
        let payload: Vec<u8> = (0u8..=255).collect(); // every byte value
        let mut records = Vec::new();
        records.push(Record {
            t_us: 1_000,
            seq: 0x1234,
            ts: 90_000,
            marker: true,
            payload: payload.to_vec(),
        });

        // Serialize: magic prefix + records.
        let mut content = Vec::new();
        content.extend_from_slice(MAGIC_PREFIX);
        for record in &records {
            content.extend_from_slice(&record.t_us.to_le_bytes());
            content.extend_from_slice(&record.seq.to_le_bytes());
            content.extend_from_slice(&record.ts.to_le_bytes());
            content.push(if record.marker { 1 } else { 0 });
            let payload_len = record.payload.len() as u16;
            content.extend_from_slice(&payload_len.to_le_bytes());
            content.extend_from_slice(&record.payload);
        }

        // Deserialize back into records.
        let mut decoded_records = Vec::new();
        let mut offset = 8; // skip magic
        while offset < content.len() {
            let t_us = u64::from_le_bytes(content[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let seq = u16::from_le_bytes(content[offset..offset + 2].try_into().unwrap());
            offset += 2;
            let ts = u32::from_le_bytes(content[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let marker = content[offset] != 0;
            offset += 1;
            let payload_len = u16::from_le_bytes(content[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;
            let payload = content[offset..offset + payload_len].to_vec();
            offset += payload_len;
            decoded_records.push(Record {
                t_us,
                seq,
                ts,
                marker,
                payload,
            });
        }

        // Verify round-trip.
        assert_eq!(records.len(), decoded_records.len());
        assert_eq!(records[0].t_us, decoded_records[0].t_us);
        assert_eq!(records[0].seq, decoded_records[0].seq);
        assert_eq!(records[0].ts, decoded_records[0].ts);
        assert_eq!(records[0].marker, decoded_records[0].marker);
        assert_eq!(records[0].payload, decoded_records[0].payload);
    }

    #[test]
    fn local_time_budget_gates_and_latches() {
        // Deterministic: a LOCAL log instance — no shared global state.
        let log = RtpDumpLog::new();
        assert!(
            log.record_internal(19_999_999, 1, 90_000, false, &[1, 2, 3]),
            "one record inside the 20 s window must be accepted"
        );
        assert!(
            !log.record_internal(20_000_000, 2, 90_000, false, &[1, 2, 3]),
            "t_us >= 20 s must be rejected"
        );
        assert!(
            !log.record_internal(19_000_000, 3, 90_000, false, &[1, 2, 3]),
            "the stop must latch: once gated, later in-window records also fail"
        );
    }

    #[test]
    fn local_byte_budget_cap_latches() {
        // Deterministic: a LOCAL log instance, so parallel tests cannot
        // reset the global's counters mid-run (the original flake).
        let log = RtpDumpLog::new();
        let payload = [0xAAu8, 0xBB]; // 17 overhead + 2 = 19 bytes per record
        let mut accepted: u64 = 0;
        let mut i: u64 = 0;
        while i < 2_000_000 {
            if log.record_internal(i, i as u16, 90_000, false, &payload) {
                accepted += 1;
                i += 1;
            } else {
                break;
            }
        }
        // 16 MiB / 19 bytes ≈ 882k records.
        assert!(
            accepted > 800_000 && accepted < 950_000,
            "expected ~883k accepted records before the 16 MiB cap, got {accepted}"
        );
        for offset in 0..1_000u64 {
            assert!(
                !log.record_internal(i + offset, 0, 90_000, false, &payload),
                "records after the byte cap must stay rejected (latched)"
            );
        }
    }

    #[test]
    fn global_flush_writes_binary_with_magic_and_record() {
        let _guard = lock_globals();
        init_rtp_dump();
        assert!(
            record(5_000, 0x1234, 90_000, true, &[0xDE, 0xAD, 0xBE, 0xEF]),
            "record inside an active session window must be accepted"
        );
        flush_rtp_dump();
        let path = std::path::Path::new("ux0:data/xcloud-rust/rtp-dump.bin");
        let content = std::fs::read(path)
            .expect("off-device flush must succeed (path is relative to the test cwd)");
        assert!(content.len() >= 8, "file must carry the magic prefix");
        assert_eq!(&content[..8], b"RTPDUMP1", "magic prefix mismatch");
        // Decode the first record: [t_us u64][seq u16][ts u32][marker u8][len u16][payload]
        let t_us = u64::from_le_bytes(content[8..16].try_into().unwrap());
        let seq = u16::from_le_bytes(content[16..18].try_into().unwrap());
        let ts = u32::from_le_bytes(content[18..22].try_into().unwrap());
        let marker = content[22];
        let len = u16::from_le_bytes(content[23..25].try_into().unwrap()) as usize;
        let payload = &content[25..25 + len];
        assert_eq!((t_us, seq, ts, marker), (5_000, 0x1234, 90_000, 1));
        assert_eq!(payload, &[0xDE, 0xAD, 0xBE, 0xEF]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn global_lifecycle_init_record_flush() {
        let _guard = lock_globals();
        init_rtp_dump();
        assert!(
            RTP_DUMP_LOG.lock().unwrap().is_some(),
            "init must publish the log"
        );
        assert!(record(6_000, 7, 90_000, false, &[1, 2, 3]));
        assert!(
            !record(20_000_000, 8, 90_000, false, &[1, 2, 3]),
            "the 20 s gate applies through the global path too"
        );
        flush_rtp_dump();
        assert!(
            !record(7_000, 9, 90_000, false, &[1, 2, 3]),
            "post-flush recording must be off (DUMP_ACTIVE cleared)"
        );
        assert!(
            RTP_DUMP_LOG.lock().unwrap().is_none(),
            "flush must clear the global log"
        );
        // Test hygiene: flush() wrote the file relative to the test cwd —
        // remove it so the repo never carries dump artifacts.
        let _ = std::fs::remove_file("ux0:data/xcloud-rust/rtp-dump.bin");
    }
}
