//! §5.3 real-capture replay: run the committed device fixture through BOTH
//! assembly paths with the capture's REAL arrival timestamps, pair the
//! outcomes by RTP timestamp, and classify every divergence — the offline
//! twin of the on-device 8a shadow comparator, with per-AU evidence.
//!
//! Fixture: `testdata/rtp-dump-dd90619.bin` — first 18.4 s of a real console
//! stream captured on-device (magic `RTPDUMP1`, little-endian records
//! `[t_us u64][seq u16][ts u32][marker u8][len u16][payload]`). Media payload
//! only; no secrets (spec §5.3).

use super::diff_fixtures::{legacy_run, pkt, LegacyEvent};
use super::*;
// 8b (§6.3): rtp.rs no longer imports this in production — import directly.
use crate::pipeline::media::assembler::seq_bounds_of;

/// Committed capture (4,370,431 bytes, 4,459 records).
const DUMP: &[u8] = include_bytes!("testdata/rtp-dump-dd90619.bin");
const DUMP_RECORDS: usize = 4_459;

struct DumpPacket {
    t_us: u64,
    seq: u16,
    ts: u32,
    marker: bool,
    payload: Vec<u8>,
}

fn parse_dump(bytes: &[u8]) -> Vec<DumpPacket> {
    assert!(bytes.len() > 8, "fixture too small");
    assert_eq!(&bytes[..8], b"RTPDUMP1", "fixture magic");
    let mut out = Vec::new();
    let mut off = 8;
    while off < bytes.len() {
        assert!(off + 17 <= bytes.len(), "truncated record header at {off}");
        let t_us = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
        let seq = u16::from_le_bytes(bytes[off + 8..off + 10].try_into().unwrap());
        let ts = u32::from_le_bytes(bytes[off + 10..off + 14].try_into().unwrap());
        let marker = bytes[off + 14] != 0;
        let len = u16::from_le_bytes(bytes[off + 15..off + 17].try_into().unwrap()) as usize;
        assert!(off + 17 + len <= bytes.len(), "truncated payload at {off}");
        out.push(DumpPacket {
            t_us,
            seq,
            ts,
            marker,
            payload: bytes[off + 17..off + 17 + len].to_vec(),
        });
        off += 17 + len;
    }
    out
}

/// Deduplicated sequence numbers of the frame with `ts` completed/abandoned
/// at packet index `upto` (stream order — one frame per timestamp).
fn frame_seqs(packets: &[Packet], ts: u32, upto: usize) -> Vec<u16> {
    let mut seqs: Vec<u16> = packets[..=upto]
        .iter()
        .filter(|p| p.header.timestamp == ts)
        .map(|p| p.header.sequence_number)
        .collect();
    seqs.sort_unstable();
    seqs.dedup();
    seqs
}

fn legacy_records(events: &[LegacyEvent], packets: &[Packet]) -> Vec<ShadowRecord> {
    events
        .iter()
        .map(|event| match event {
            LegacyEvent::Complete { at, ts, data } => {
                let seqs = frame_seqs(packets, *ts, *at);
                let (first_seq, last_seq) = seq_bounds_of(&seqs);
                ShadowRecord {
                    ts: *ts,
                    class: ShadowClass::Complete,
                    first_seq,
                    last_seq,
                    packet_count: seqs.len() as u16,
                    bytes_hash: Some(hash_bytes(data)),
                    digests: Box::new([]),
                    data: bytes::Bytes::copy_from_slice(data),
                }
            }
            LegacyEvent::Abandoned { at, ts } => {
                let seqs = frame_seqs(packets, *ts, *at);
                let (first_seq, last_seq) = seq_bounds_of(&seqs);
                ShadowRecord {
                    ts: *ts,
                    class: ShadowClass::Abandoned,
                    first_seq,
                    last_seq,
                    packet_count: seqs.len() as u16,
                    bytes_hash: None,
                    digests: Box::new([]),
                    data: bytes::Bytes::new(),
                }
            }
            LegacyEvent::Invalid { at, ts } => {
                let seqs = frame_seqs(packets, *ts, *at);
                let (first_seq, last_seq) = seq_bounds_of(&seqs);
                ShadowRecord {
                    ts: *ts,
                    class: ShadowClass::Invalid,
                    first_seq,
                    last_seq,
                    packet_count: seqs.len() as u16,
                    bytes_hash: None,
                    digests: Box::new([]),
                    data: bytes::Bytes::new(),
                }
            }
        })
        .collect()
}

struct NewRec {
    rec: ShadowRecord,
    /// True when the outcome was returned by `flush_due` (the 12 ms hold).
    via_hold: bool,
}

fn new_records(dump: &[DumpPacket]) -> Vec<NewRec> {
    let mut assembler = AccessUnitAssembler::new();
    let mut out = Vec::new();
    for p in dump {
        if let Some(outcome) = assembler.push(p.seq, p.ts, p.marker, &p.payload, p.t_us) {
            out.push(NewRec {
                rec: ShadowRecord::from_new(outcome, Box::new([])),
                via_hold: false,
            });
        }
        while let Some(outcome) = assembler.poll_outcome() {
            out.push(NewRec {
                rec: ShadowRecord::from_new(outcome, Box::new([])),
                via_hold: false,
            });
        }
        if let Some(outcome) = assembler.flush_due(p.t_us) {
            out.push(NewRec {
                rec: ShadowRecord::from_new(outcome, Box::new([])),
                via_hold: true,
            });
        }
        while let Some(outcome) = assembler.poll_outcome() {
            out.push(NewRec {
                rec: ShadowRecord::from_new(outcome, Box::new([])),
                via_hold: false,
            });
        }
    }
    out
}

struct Pair {
    legacy: ShadowRecord,
    new_rec: ShadowRecord,
    via_hold: bool,
}

fn pair_up(legacy: &[ShadowRecord], news: &[NewRec]) -> (Vec<Pair>, usize, usize) {
    let mut pairs = Vec::new();
    let mut new_used = vec![false; news.len()];
    let mut legacy_used = vec![false; legacy.len()];
    for (i, l) in legacy.iter().enumerate() {
        let mut found = None;
        for (j, n) in news.iter().enumerate() {
            if !new_used[j] && n.rec.ts == l.ts {
                found = Some(j);
                break;
            }
        }
        if let Some(j) = found {
            new_used[j] = true;
            legacy_used[i] = true;
            pairs.push(Pair {
                legacy: l.clone(),
                new_rec: news[j].rec.clone(),
                via_hold: news[j].via_hold,
            });
        }
    }
    let unmatched_legacy = legacy_used.iter().filter(|used| !**used).count();
    let unmatched_new = new_used.iter().filter(|used| !**used).count();
    (pairs, unmatched_legacy, unmatched_new)
}

#[test]
fn replay_real_capture_through_both_paths() {
    let dump = parse_dump(DUMP);
    assert_eq!(dump.len(), DUMP_RECORDS, "fixture record count");

    let packets: Vec<Packet> = dump
        .iter()
        .map(|p| pkt(p.seq, p.ts, p.marker, &p.payload))
        .collect();

    // ---- legacy path (mirrors receive()'s control flow) --------------------
    let events = legacy_run(&packets);

    // Gate: every legacy-submitted AU must be Annex-B (au_no_startcode==0
    // evidence, reproduced offline).
    for event in &events {
        if let LegacyEvent::Complete { data, ts, .. } = event {
            assert!(
                data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]),
                "legacy submitted non-Annex-B AU at ts {ts}"
            );
        }
    }

    // ---- new path (real arrival timestamps, hold live) ---------------------
    let legacy_recs = legacy_records(&events, &packets);
    let new_recs = new_records(&dump);
    let (pairs, unmatched_legacy, unmatched_new) = pair_up(&legacy_recs, &new_recs);

    // ---- classify every pair ----------------------------------------------
    let mut match_count = 0usize;
    let mut bytes_mismatch = 0usize;
    let mut class_mismatch = 0usize;
    let mut bucket1 = 0usize;
    let mut bucket2 = 0usize;
    let mut bound_mismatch = 0usize;
    let mut bound_non_bucket1 = 0usize;
    let mut reject_artifact = 0usize;
    let mut hold_bytes = 0usize;
    let mut unexplained_bytes = Vec::new();
    let mut bound_breakdown: Vec<(ShadowClass, ShadowClass, bool, usize)> = Vec::new();

    for pair in &pairs {
        let verdict = classify_pair(&pair.legacy, &pair.new_rec);
        let same_bounds = (pair.legacy.first_seq, pair.legacy.last_seq, pair.legacy.packet_count)
            == (
                pair.new_rec.first_seq,
                pair.new_rec.last_seq,
                pair.new_rec.packet_count,
            );
        let bounds_compared =
            pair.legacy.packet_count > 0 && pair.new_rec.packet_count > 0;
        if bounds_compared && !same_bounds {
            // §11.3a: both-reject pairs are exempt from the boundary gate and
            // counted separately (mirror of ShadowComparator::compare).
            let both_reject = matches!(
                pair.legacy.class,
                ShadowClass::Abandoned | ShadowClass::Invalid
            ) && matches!(
                pair.new_rec.class,
                ShadowClass::Damaged | ShadowClass::Discarded
            );
            if both_reject {
                reject_artifact += 1;
            } else {
                bound_mismatch += 1;
                if verdict != PairVerdict::BucketDamagedOldComplete {
                    bound_non_bucket1 += 1;
                }
            }
            let key = (pair.legacy.class, pair.new_rec.class, pair.via_hold);
            match bound_breakdown.iter_mut().find(|k| k.0 == key.0 && k.1 == key.1 && k.2 == key.2) {
                Some(entry) => entry.3 += 1,
                None => bound_breakdown.push((key.0, key.1, key.2, 1)),
            }
        }
        match verdict {
            PairVerdict::Match => match_count += 1,
            PairVerdict::BytesMismatch => {
                bytes_mismatch += 1;
                if pair.via_hold {
                    hold_bytes += 1;
                }
                unexplained_bytes.push(pair);
            }
            PairVerdict::ClassMismatch => class_mismatch += 1,
            PairVerdict::BucketDamagedOldComplete => bucket1 += 1,
            PairVerdict::BucketCompleteOldAbandon => bucket2 += 1,
        }
    }

    // ---- evidence ----------------------------------------------------------
    println!("=== §5.3 replay evidence (fixture dd90619, first 18.4 s) ===");
    println!(
        "records={} legacy_events={} new_outcomes={} pairs={} unmatched_legacy={} unmatched_new={}",
        dump.len(),
        legacy_recs.len(),
        new_recs.len(),
        pairs.len(),
        unmatched_legacy,
        unmatched_new
    );
    println!(
        "match={match_count} bytes_mismatch={bytes_mismatch} (via_hold={hold_bytes}) class_mismatch={class_mismatch} bucket1={bucket1} bucket2={bucket2} gated_bound={bound_mismatch} (outside bucket1={bound_non_bucket1}) reject_artifact={reject_artifact}"
    );
    println!("bound_mismatch breakdown (legacy_class, new_class, via_hold, count):");
    for (l, n, hold, count) in &bound_breakdown {
        println!("   {l:?} -> {n:?} via_hold={hold}: {count}");
    }
    for pair in unexplained_bytes.iter().take(30) {
        println!(
            "UNEXPLAINED bytes mismatch: ts={} legacy=({},{},{}B) new=({},{},{}B)",
            pair.legacy.ts,
            pair.legacy.first_seq,
            pair.legacy.last_seq,
            pair.legacy.packet_count,
            pair.new_rec.first_seq,
            pair.new_rec.last_seq,
            pair.new_rec.packet_count,
        );
    }

    // ---- gates (§11.3 amended precondition, replay edition) ---------------
    assert!(
        class_mismatch == 0,
        "unexpected class pairs: {class_mismatch}"
    );
    assert_eq!(
        bytes_mismatch, 0,
        "§11.3: byte mismatches must reach 0 after the assembler fix"
    );
    assert_eq!(
        bound_non_bucket1, 0,
        "§11.3: bound mismatches only in bucket 1 (lost leading packet)"
    );
    assert_eq!(
        unmatched_legacy, 0,
        "legacy events with no new counterpart"
    );
    assert_eq!(
        unmatched_new, 0,
        "§11.5: unused new records after the rule-4 fix = defect signal"
    );
    // §11.3a: both-reject boundary artifacts are exempt but rate-limited —
    // the fixture shows 1 in 18.4 s; well above ~2 means something else broke.
    assert!(
        reject_artifact <= 2,
        "§11.3a: reject-bound artifacts above ~2 per 18 s of replay: {reject_artifact}"
    );
    assert!(
        unexplained_bytes.is_empty(),
        "byte mismatches present (see above)"
    );
}

/// 8b gate diagnosis: bucket-1 pairs (new = Damaged where legacy = Complete)
/// are the frames the authority switch now DISCARDS (device: ~0.8/s in every
/// d777842 baseline session; artefact/resync regression at 727ce76). Same
/// dual-path replay as the gate test, but keeps the raw AuDescriptor and
/// attributes every case to the `is_complete` clause that failed:
/// `marker_seen && holes == 0 && packet_count > 0 && had_first_slice`.
/// `B1_DUMP` env var overrides the fixture path (fresh-capture re-runs).
#[test]
fn dump_bucket1_clause_failures() {
    let owned: Vec<u8> = match std::env::var("B1_DUMP") {
        Ok(p) if !p.is_empty() => {
            std::fs::read(&p).unwrap_or_else(|e| panic!("B1_DUMP {p}: {e}"))
        }
        _ => DUMP.to_vec(),
    };
    let dump = parse_dump(&owned);
    let packets: Vec<Packet> = dump
        .iter()
        .map(|p| pkt(p.seq, p.ts, p.marker, &p.payload))
        .collect();
    let events = legacy_run(&packets);
    let legacy_recs = legacy_records(&events, &packets);
    let legacy_complete: std::collections::HashMap<u32, &ShadowRecord> = legacy_recs
        .iter()
        .filter(|r| r.class == ShadowClass::Complete)
        .map(|r| (r.ts, r))
        .collect();

    // New side, keeping descriptors this time.
    let mut assembler = AccessUnitAssembler::new();
    let mut outcomes: Vec<(AssemblyOutcome, bool)> = Vec::new();
    for p in &dump {
        if let Some(o) = assembler.push(p.seq, p.ts, p.marker, &p.payload, p.t_us) {
            outcomes.push((o, false));
        }
        while let Some(o) = assembler.poll_outcome() {
            outcomes.push((o, false));
        }
        if let Some(o) = assembler.flush_due(p.t_us) {
            outcomes.push((o, true));
        }
        while let Some(o) = assembler.poll_outcome() {
            outcomes.push((o, false));
        }
    }

    // Histogram of failing clauses (a case may fail several).
    let (mut no_marker, mut has_holes, mut no_count, mut no_first, mut cases) =
        (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut examples = 0u32;
    for (outcome, via_hold) in &outcomes {
        let AssemblyOutcome::Damaged(desc) = outcome else {
            continue;
        };
        if !legacy_complete.contains_key(&desc.rtp_ts) {
            continue; // not bucket 1 (legacy did not complete this ts)
        }
        cases += 1;
        let f_marker = !desc.marker_seen;
        let f_holes = desc.holes > 0;
        let f_count = desc.packet_count == 0;
        let f_first = !desc.had_first_slice;
        no_marker += f_marker as u32;
        has_holes += f_holes as u32;
        no_count += f_count as u32;
        no_first += f_first as u32;
        if examples < 12 {
            examples += 1;
            let lrec = legacy_complete[&desc.rtp_ts];
            let lens: Vec<String> = lrec
                .digests
                .iter()
                .map(|d| format!("{}:{}B", d.seq, d.len))
                .collect();
            // Marker timing: delay from the frame's first packet to its
            // marker packet (per the dump), and the AU's age at resolve
            // (arrival of last stored packet's frame — proxy: first->marker).
            let frame_pkts: Vec<&DumpPacket> =
                dump.iter().filter(|p| p.ts == desc.rtp_ts).collect();
            let first_t = frame_pkts.first().map(|p| p.t_us).unwrap_or(0);
            let marker_delay = frame_pkts
                .iter()
                .find(|p| p.marker)
                .map(|p| p.t_us.saturating_sub(first_t));
            println!(
                "B1#{examples} ts={} t0_us={} via_hold={}: failed=[{}{}] holes={} count={} first_seq={} last_seq={} had_first_slice={} marker={} || legacy=({}-{} x{} {}B) head_match={} marker_delay_us={:?} legacy_lens=[{}]",
                desc.rtp_ts,
                first_t,
                via_hold,
                if f_marker { "marker " } else { "" },
                if f_first { "first_slice " } else { "" },
                if f_holes { format!("holes={} ", desc.holes) } else { String::new() },
                desc.packet_count,
                desc.first_seq,
                desc.last_seq,
                desc.had_first_slice,
                desc.marker_seen,
                lrec.first_seq,
                lrec.last_seq,
                lrec.packet_count,
                lrec.data.len(),
                lrec.first_seq == desc.first_seq,
                marker_delay,
                lens.join(","),
            );
        }
    }
    println!("=== bucket-1 clause histogram (fixture 18.4 s) ===");
    println!(
        "cases={cases} no_marker={no_marker} holes>0={has_holes} count==0={no_count} !had_first_slice={no_first}"
    );
    // The diagnosis is data, not a verdict: assert we actually saw cases so
    // the histogram cannot silently become vacuous.
    assert!(cases > 0, "fixture shows zero bucket-1 cases — cannot diagnose");
}
