# GreenVita TODO

## Open Issues

### 1. Latency Accumulation After 30+ min Sessions
- **Status (2026-10-12): NOT REPRODUCED on 0.2.5.** Two measured sessions, both
  with the aggressive feedback toggles OFF, 3000 kbps cap, Baseline, queue=1:
  - **12.1 min, game content, ~59.5 fps, no input:** `lag` mean 1.27 ms (slope
    -0.36 ms/min), `ageMax` flat 23-37 ms, 0 resyncs, 0 decode errors, Xbox RR
    loss 0.0%. Drops 2.65/min, all console AU skips.
  - **37.4 min, movie at 24 fps, no input:** `lag` mean 1.16 ms (slope
    -0.134 ms/min = **-5 ms over the whole session**), `ageMax` mean 5.7 ms
    (slope -0.106 ms/min), `display_pickup` 8.4 ms flat, 0 resyncs / 0 errors,
    drops 0.51/min. User reported **no perceptible latency after 37 min**,
    including a few seconds of input at the end.
  - The old drift floor (~0.25-0.5 s/min) predicts **+9 to +18 s** over 37 min.
    Measured: none, in any client-side or transport-side signal.
  - **REPRODUCED (16.9 min, racing game, 4000 kbps cap, ~58.5 fps):** steady
    monotonic growth - user: *"every minute a little bit worse until it was just
    very bad"* - plus early freezes / frame drops and heavy artefacts.
    Metrics: drops **20.1/min** (340 total), flushes **29** (one every ~20 s,
    sometimes only 6 s apart), resyncs 5, rtp_gaps 124, bandwidth pegged at the
    4000 cap. `lag` slope -0.74 ms/min and `ageMax` +0.03 ms/min stayed FLAT,
    which is the structural blind spot: RTP-schedule slip cannot see pre-stamp
    queue growth, so this cannot be used to argue glass latency was flat.
    Attribution: 4000 kbps very likely saturates the ~8 Mbps-usable 2.4 GHz
    airtime for high-motion content.
- **Longest hardware-validated clean session: 15.2 min (Main/CABAC, 4000 kbps,
  decode queue depth 1, no accumulation).** A session longer than 30 minutes
  with active input is still the outstanding validation for this item.
  - **Experiment armed:** `FLUSH_TRIGGER_ENABLED = false` in `session.rs` disables
    the flush trigger entirely for the next run, so the console is never asked to
    rebuild its encoder. If artefacts stop but the steady drift continues, the
    drift is console-side and the flushes only caused the glitches; if both stop,
    the flushes were causing the drift too.

- **HARDWARE BISECT (three builds, real hardware, same content):**
  - `888c1e0` (0.2.4 baseline, pre-0.2.5): GOOD. At a 2500 kbps cap: no artefacts,
    very low latency. Also held up at 4000 and even uncapped, settling 4000-5500
    kbps, artefacts only when spiking to ~8000.
  - `256bac0` (first 0.2.5 commit): GOOD. No perceptible latency accumulation,
    artefacts low.
  - `e988430` (0.2.5 tip): BAD. Artefacts plus latency accumulation.
  => The regression is not inherited from upstream/master; it was introduced after
     `256bac0`.

- **EXONERATED:**
  - `256bac0`: its `session.rs` change is behaviourally inert
    (`LAG_FLUSH_THRESHOLD_US` = 75 ms, so with the metric previously dead the lag
    term was already false; `drops_since_flush` is monotonic between flushes, so
    the new `lag_high_since = None` reset never behaves differently from baseline).
    Its `flush_expired` stall path never fired in any run (`rtp_hold_expired`
    stayed 0). Per-packet cost is one extra `Instant::now()` in
    `PendingVideoFrame::insert`. It DOES newly make `stream_lag_us` live, which
    `adjust_remb_dynamically` consumes (REMB cut at >100 ms / >50 ms, and the
    `conditions_good` recovery gate requires `stream_lag_us < 30_000`) - a
    genuinely new coupling, but `256bac0` measured good, so it is not the
    regression.
  - `416a66f` (debug overlay): UI-only, trivially cheap - one extra `rect_filled`
    plus ~3 extra `format!` per frame in the UI loop. Cannot cause RTP assembly
    drops or a load-dependent latency ramp.
  - The per-second `eprintln!("{}", self.status)`: pre-existing (introduced in
    `989b62a`) and present in the good `256bac0`, so stderr I/O on the pump
    thread is not the cause.
  - `a6545c1` (SDP fmtp merge / `max-fr`): postdates `a7b70ea`, which was already
    bad, so it cannot explain the regression.
  - The flush trigger: falsified as the cause - artefacts appeared in a run with
    zero flushes, and the trigger's behaviour is unchanged from the baseline.

- **PRIME SUSPECT (strong hypothesis, not proven):** `a7b70ea` added a
  once-per-second blocking `writeln!` + `flush()` of the metrics CSV to `ux0:`
  (memory card) on the `green-vita-rtc` pump thread, called from
  `VideoReceiver::status()`. That thread drains incoming RTP, sends RTCP feedback
  and submits access units to the decoder. A once-a-second stall becomes a burst
  of buffered RTP on resume, and the AU assembly window is only 12.5 ms, so
  higher bitrate makes blowing it likelier -> dropped AU -> broken reference
  chain -> artefacts. The high flush/drop counts seen at a 4000 cap are a
  CONSEQUENCE of the stall, not the cause.

- **FIX ALREADY COMMITTED:** `966f885` "0.2.5: move metrics CSV I/O off the RTC
  pump thread; stop stderr spam there". `metrics_log.rs::open()` still creates
  the file and writes the header once per session, then spawns a
  `"green-vita-metrics-log"` thread owning the file; `write_row` only formats a
  row and sends it over an unbounded channel, doing no file I/O, so it cannot
  block the pump. `session.rs` gates the per-second status `eprintln!` behind
  `LOG_STATUS_TO_STDERR = false`.

- **CRITICAL CAVEAT:** every 0.2.5 metrics CSV currently on record was produced by
  the perturbing build — that includes the "latency NOT reproduced" sessions
  (12.1 min game / 37.4 min movie) recorded above — so those specific numbers
  must be re-taken before being cited again.

- **Builds for re-testing** live in `/Users/mk/git/vita-xbox/dist` (git-ignored,
  local only) and are flashed with `bash scripts/docker-build.sh deploy <ip>
  <dist-name>`:
  - `green-vita-0.2.5-e988430-20261012-221600` (the `966f885` fix build,
    currently on the device)
  - `green-vita-0.2.5-256bac0-20261012-224541` (known good)
  - `green-vita-0.2.4-888c1e0-20261012-222527` (0.2.4 baseline)

- **STATUS:** CONFIRMED. The blocking once-per-second CSV write on the pump thread
  is the regression cause. The earlier flush-trigger → `dimensionschanged` →
  artefacts theory is dead: both the artefacts and the drift were downstream of
  the pump-thread stall.

- **Confirmation run:** `green-vita-0.2.5-e988430-20261012-221600` (the `966f885`
  fix build), racing game, **4000 kbps cap, baseline profile** — the exact
  conditions under which `e988430` had previously degraded monotonically (the
  16.9 min session described as "every minute a little bit worse"). Result now:
  **latency low, no perceptible accumulation**, and only **minor artefacts**,
  which the user cleared by turning PLI on. This was a SHORT race, not the 30+
  min condition, so "fixed" vs "slower onset" is not fully discriminated; the
  30+ min active-input session remains untested on the fixed build.

- **Still untested**: sustained ACTIVE INPUT over 30+ min (both measured sessions
  were input-free; input packet rate is the last remaining variable), and an A/B
  of the aggressive toggles.
- **New hypothesis**: the drift may have been **self-inflicted by the aggressive
  feedback knobs** - periodic keyframe PLI (200 ms), TWCC delay injection, REMB
  interval 100 ms. The test config listed at the bottom of this file enabled all
  of them; both clean sessions ran with them OFF. The README itself warns that
  PLI spam "can overload encoder and worsen backlog".
- **Measurement fixes landed in 0.2.5**:
  - `METRICS.stream_lag_us` was **never written anywhere** (declared, drained and
    displayed, but no store): HUD `lag:`/`lagMax:` were permanently 0 and the lag
    half of the flush trigger could never fire. Now computed per access unit from
    RTP schedule slip. The lag trigger is gated behind `LAG_FLUSH_ENABLED = false`
    (observe-only build); the drop trigger is live.
  - **Drift fix (2026-10-13)**: the lag estimator used a single session-long
    anchor, so a fixed rate difference between the console's RTP clock and the
    Vita monotonic clock accumulated linearly (measured `-1.51 ms/min` on the
    15.2 min Main session, ~25 ppm ~= 90 ms/h) and was indistinguishable from real
    queue growth. It now reports the slip relative to the best access unit of the
    current 60 s window, re-anchoring at each window boundary and on any
    forward/backward RTP timeline jump (a backward jump previously pinned `lag` at
    0 for the rest of the session; a forward jump produced a phantom spike).
    Consequence: `lag` now measures *growth* above the recent floor, so a steady
    non-growing queue reads 0, and the 59.6 ms startup blip is gone. `lagMax`
    keeps its per-second-max meaning. Validate with a >=30 min session.
  - `maybe_flush_backlog` never cleared `lag_high_since` when not eligible, so a
    single eligible instant permanently satisfied `LAG_FLUSH_SUSTAIN`.
  - `rtp::flush_expired` was an empty stub; now a stall-based expiry (100 ms from
    the LAST packet, marker-absent only). **0 firings in 49 min measured -
    effectively inert at current rates.**
  - Per-second metrics CSV + `scripts/analyze_metrics.py` (see Testing Notes).
  - **Capture-to-arrival metric (2026-10-13, for 0.2.6)**: `capture_to_arrival_us`
    and `capture_to_arrival_max_us` derived from RTCP Sender Report (NTP + RTP
    timestamp pair) anchors the console's capture clock to local time. This metric
    is **NOT blind to the console's pre-RTP-stamp queue** — if it grows while
    `stream_lag_us` stays flat, accumulation is proven console-side. Added to CSV,
    HUD (via `analyze_metrics.py` trends/verdict), and per-window table.
- **Open defects found while measuring**:
  - `rtcp_rr_loss_fraction` is stored as percent but read back as
    `stored / 100.0` and compared against 5.0/2.0/1.0 => the loss-based REMB
    reduction can never fire.
  - `constrain_h264_codec` appends a second `a=fmtp` line hardcoding
    `profile-level-id=42e01f` after the negotiated one (see item 2), and `max-fr`
    is a constant: `video_fps * 90_000 / video_fps` == 90 000.
  - Dead counters never incremented anywhere, so HUD `hold:S` and several CSV
    columns are structurally 0: `rtp_abandons`, `rtp_hold_saved`,
    `rtp_wait_drops`, `rtp_invalid`, `dec_queue_full`, `video_metadata_sent`.
- **Suspected root cause** (unchanged as a hypothesis): console-side send buffer
  buildup. Note `lag` is structurally blind to it; `ageMax`/`display_pickup` are
  client-side and were also flat.
- **Next steps**:
  - 30+ min game session with continuous input (the untested variable).
  - Compare the flush-disabled run against the 4000 kbps run above (same
    workload ideally): do the artefacts/freezes stop?
  - Drop the bitrate cap back to <=3000 for high-motion content; 4000 pegged the
    link and multiplied drops ~8x.
  - Add a **real inbound packet-loss metric**. Xbox's RTCP RR reports 0.0% loss
    but those report blocks describe a stream it would receive FROM us, which
    does not exist, so `rr%` is meaningless. `rtp_gaps` (assembly timeouts) is
    only a proxy. A sequence-number-based loss/late counter is needed.
  - A/B the aggressive toggles against the same workload.
  - If drift reproduces, add a better drift detector (`ageMax`/`pick` or the
    metadata round-trip) since `lag` cannot see the pre-stamp queue.

### 2. Dark Scene Blockiness (Baseline Profile Limitation) — RESOLVED ✅ HARDWARE-VERIFIED
- **Issue**: Dark areas with subtle gradients show heavy blocking/banding
- **Root cause**: Baseline lacks CABAC entropy coding
- **Resolution (2026-10-13)**: The console's encoder DOES support Main profile.
  Proven by capturing a real Chrome xHome session (webrtc-internals dump):
  - Chrome offers Main as `profile-level-id=4d001f` (no constraint flags)
  - The console ANSWERS that PT with `profile-level-id=4d002a` (Main, level 4.2)
  - The console's answer contains NO High PT, even though Chrome offered one
  - Our bug: we declared Main as `4de01f` (constraint_set0=1 = Baseline-compatible)
    so the console matched our Baseline fallback PT (`42e01f`) instead.
  - Fix: `peer.rs::h264_profile_level_id` now returns `4d001f`/`4d0020` for Main.
    This brings CABAC, 8x8 transform, and weighted prediction — exactly what
    dark/flat gradients need.
- **Status**: Fixed in code (commit b9cbe82). **Hardware-verified 2026-10-13**:
  a 15.2-minute session (Main profile, level 4.2, 4000 kbps cap, decode queue
  depth 1) produced **`decode_errors 0` for the entire session** — the Vita's
  hardware decoder handles Main/CABAC cleanly **even at the shallowest queue
  depth**. This proves that a deeper queue is NOT a prerequisite for using
  Main. The earlier belief that queue depth 3+ was mandatory came from the
  concern that Main might carry B-frames requiring reordering; the measured
  sessions have shown no B-frames. Note that decode queue depth remains an
  independent latency/artefact trade-off knob (a deeper queue raises the
  internal `pending_limit` in `src/streaming/video/worker.rs::submit_access_unit`,
  so fewer oldest-access-unit drops occur when the queue fills, at the cost of
  added latency) — but it is unrelated to whether Main works.
  **User-observed outcome of this session: no latency accumulation, but minor
  artefacts and perceived visual quality drops.**
  - Console answer SDP for the Main session:
    ```
    m=video 9 UDP/TLS/RTP/SAVPF 102 103
    a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=4d002a
    a=fmtp:103 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e02a
    ```
    i.e. Main (level 4.2) accepted on primary PT 102, Constrained Baseline
    fallback also accepted at PT 103.
  - Session metrics: fps 59.6–59.9 steady (50.7 first window), lag 0.0–1.4 ms
    after 59.6 ms startup blip (least-squares slope −1.51 ms/min — no
    accumulation), resyncs 1 (t=8 s startup), drops 103 total (6.77/min, 82 in
    first window), queue_full 0.45/s decaying to 0.0, flushes 0, resets 0,
    stall expiries 0, rtp_gaps 15, rtcp_rr_loss_fraction 0.00.
  - Bandwidth mean 3469 kbps, per-minute windows up to 4024 — riding the 4000
    cap, versus 2877 kbps at a 3500 cap in earlier Baseline sessions.
- **Console H.264 capability set (measured)**: exactly Baseline (`4200`),
  Constrained Baseline (`42e0`), Main (`4d00`). **High (`6400`) and High 4:4:4
  (`f400`) are NOT supported**.
- **Evidence A** (Chrome capture, `chrome-dumps/webrtc_internals_dump.txt`,
  `PeerConnections['23-2'].updateLog[11]` = Chrome offer, `updateLog[27]` = console
  answer): Chrome offered H.264 at PTs 39 (`4d001f`, pm=0), 41/43 (`f4001f`),
  102/104 (`42001f`), 108/114 (`42e01f`), 116 (`4d001f`, pm=1) and 118/120
  (`64001f`). Console answered only 39, 102, 104, 108, 114, 116 — omitting 41,
  43, 118, 120. It answered Main as `4d002a` (matches on profile_idc + constraint
  flags, ignores level, raises level to encoder needs).
- **Evidence B** (real Vita session, High selected in settings): our offer had PT
  102 (`profile-level-id=64001f`, High) + PT 103 (`42e01f`, Constrained Baseline
  fallback). Console answered **only PT 103** with `profile-level-id=42e02a` — it
  refused the High payload type and accepted the Baseline fallback. The fallback
  is why nothing failed loudly; "no decode errors" is NOT evidence High was
  negotiated.
- **Evidence C** (why Main also silently downgraded before b9cbe82): our offer
  declared `4de01f` (constraint_set0=1 = Baseline-compatible), so that PT matched
  the console's Baseline capability and the console answered Baseline at the SAME
  PT number (102) with `42e02a`. Fixed to `4d001f` in commit b9cbe82.
- **Verification method** (a profile downgrade is silent): the ONLY way to confirm
  what was actually negotiated is the answer SDP dumped to the device at
  `ux0:data/xcloud-rust/last-answer.sdp` (pull over FTP).
  - `profile-level-id=4d002a` = Main accepted
  - `profile-level-id=42e02a` = Constrained Baseline (a downgrade)
  - `profile-level-id=64002a` = High accepted (never observed)
- **Our fmtp extras** (`max-fr`, `max-fs`, `max-qp`) are NOT the cause of these
  refusals — Chrome's clean High PT (none of those params) was refused too.
- **Note**: Main is the ceiling. High entries (`64001f`/`640020`) kept as-is for
  parity with Chrome's offer but will not be negotiated. **Main/CABAC is now
  verified working on the Vita HW decoder (`decode_errors 0`)**.

### 3. RSSI-Based Adaptive Bitrate (Disabled - NetCtl Crash)
- **Issue**: `sceNetCtlInit()` causes hard crash on some Vita units
- **Workaround**: Feature disabled, app starts cleanly
- **Future**: Investigate lazy init or alternative WiFi signal detection

### 4. RTCP RR/SR RTT Calculation
- **Current**: Parses SenderReport NTP timestamp but doesn't compute RTT
- **Needed**: Track our RR send time → compute RTT from SR NTP
- **Use**: Adaptive REMB interval based on RTT

### 5. Smart Non-Reference Frame Dropping
- **When lag > 50ms**: Drop B-frames / non-ref P-frames before decode queue
- **Preserve**: IDR, SPS, PPS, reference P-frames
- **Benefit**: Reduces decoder load 30-50% without full resync

### 6. Frame Deadline Dropping
- **If frame can't meet next VSYNC**: Drop it early
- **Prevents**: "Zombie frames" clogging pipeline when already late

### 7. Adaptive Decode Queue Depth
- **High jitter/lag** → queue=1
- **Normal** → user configured depth
- **Benefit**: Less buffered latency during congestion

### 8. No RTX payload type and no generic NACK (loss-induced artefacts) — PARTIALLY IMPLEMENTED
- **Measured evidence (Chrome xHome, LAN, inbound shaped to 1.5 Mbit/s via dnctl/pf):**
  - 1280x720@60 stream, 32715 packets, 1532 lost (~4.7% loss)
  - nackCount 499, retransmittedPacketsReceived 1809 (1,426,756 bytes ≈ 5.5% of stream recovered)
  - pliCount 38, firCount 0 (PLI is last resort)
  - freezeCount 10, totalFreezesDuration 12.135 s (Chrome HOLDS last good frame)
  - framesDecoded 19146, framesDropped 98, jitter 0.009
  - framesAssembledFromMultiplePackets 6350, totalAssemblyTime 93.99 s
  - Subjective: video looked very bad at 0.6-1 Mbps but NO artefacts; after 1-2 min loading, latency/freezes then caught up in 10-15 s to very low latency; gameplay smooth with lower latency than Vita has ever achieved.
- **Conclusions:**
  1. Console DOES honour NACK + RTX retransmission — 1809 packets retransmitted and received.
  2. Chrome HOLDS the last good frame instead of rendering corruption (10 freezes, 12.1 s total); our client presents damaged frames → persistent macroblocking until next IDR.
  3. Keyframe requests are a last resort for Chrome (38 PLIs vs 499 NACKs); our only recovery tool is the keyframe request, and an IDR at low bitrate is enormous — which is why periodic PLI measurably made things worse.
- **Raw evidence location:** `/Users/mk/git/vita-xbox/chrome-dumps/October 13 1307/`
- **Implementation status (2026-10-13):**
  - ✅ **Stage 1 - Advertise capability & generate NACKs (commit 11674a5 + this work):** Generic `a=rtcp-fb:<pt> nack` added to both H.264 codecs; RTX payload types 112/113 registered with `apt=102`/`apt=103`. NACK generator interceptor (`rtc_interceptor::nack::generator::NackGeneratorInterceptor`) wired into the interceptor registry (`feedback_registry()` in `src/api/streaming/rtc/peer.rs`). Activation condition satisfied: codec's `rtcp_feedback` contains `RTCPFeedback { typ: "nack", parameter: "" }` (generic NACK, empty parameter). The interceptor automatically binds to remote video streams that advertise generic NACK and generates RTCP TransportLayerNack packets for missing sequences.
  - ✅ **Stage 2 - RTX receive:** 
    - (a) Negotiated RTX payload types extracted from console's ANSWER SDP via `extract_negotiated_rtx_mapping()` in `src/api_xbox/streaming/rtc/sdp.rs`. Parses `a=rtpmap:<pt> rtx/90000` + `a=fmtp:<pt> apt=<h264-pt>` and publishes mapping globally via `set_negotiated_rtx_map()` (called from `session.rs::set_remote_answer()`).
    - (b) RTX unwrapping implemented in `VideoRtp::receive_unwrapped_rtx()` (`src/api/streaming/rtc/rtp.rs`): detects negotiated RTX payload types, reads 2-byte original sequence number (big-endian, RFC 4588), feeds remaining payload into normal reassembly path using original sequence number. RTX packets do not disturb main stream's sequence accounting, gap/damage detection, stream-lag metric, or decoder queue.
    - (c) RTX packets are pure duplicates — they only fill gaps in pending frames, never trigger keyframe requests or damage scoring on their own.
  - ✅ **Stage 3 - Counters for hardware verification:** Added `rtcp_nack_sent` (sequence numbers requested for retransmission, approximated from assembly-timeout gaps) and `rtx_packets_received` (RTX packets successfully unwrapped) to `VideoMetrics`, CSV header (now 48 columns), and `analyze_metrics.py` (window table + totals). Script degrades gracefully on older CSVs.
  - ⬜ **Stage 4 - Hold last good frame (secondary):** Not yet implemented. Chrome's freeze behaviour (hold last good frame on unrecoverable damage) would require changes to the presentation path (`src/shell/surface.rs`) and damage detection (detect whole missing AUs via RTP timestamp jumps).
- **Acceptance criteria (for Stages 1-3):**
  - At ~1 Mbps with several percent loss, metrics CSV must show `rtcp_nack_sent` and `rtx_packets_received` climbing.
  - Console's retransmissions must actually arrive — verifiable on hardware by watching those counters climb.
  - Subjective: artefacts should stop persisting for seconds at low bitrate; picture should hold/freeze instead of showing macroblocking (pending Stage 4).

### 9. 30/60 fps switch: only untried lever is clientdevicecapabilities
- The switch is inert through every path tried so far. It feeds the SDP
  `a=framerate`, the SDP `max-fr` (via `constrain_h264_codec`) and the
  control-channel `VideoPreference.fps` - all measured ignored.
- It ALSO selects the `level_idc` byte of the `profile-level-id`
  (`4d001f` = level 3.1 = 1280x720@30 for 30 fps; `4d0020` = level 3.2 =
  1280x720@60 for 60 fps), and that is ignored too: the console answers
  `4d002a` (level 4.2) regardless, and every session so far declared level
  3.1 while the console sent 58-70 fps - which level 3.1 forbids. Our
  declared level is in fact below what the console needs, another sign it
  is disregarded.
- The one untried lever: `/streaming/characteristics/clientdevicecapabilities`,
  which `startup_messages` in
  `src/api_xbox/streaming/control/channel.rs` deliberately sends as an empty
  object `json!({})`, with a code comment saying the full form
  (`supportsFps`/`maxWidth`/`maxBitrateKbps`) is legacy and "may push the
  console into conservative pacing".
- Why it is worth retrying now: the console has been proven
  capability-driven - it accepted Main profile the moment we declared it
  correctly, and it mirrors RTX/nack/FEC/extmaps from the client's offer.
- Caveats: the exact expected payload shape is unrecorded (the code comment
  is the only reference and there is no local clone of the reference clients
  to check - fetch from GitHub first), the conservative-pacing risk is real,
  and the payoff is bandwidth/smoothness (the ~10 frames/s the console sends
  above the 60 Hz panel), not picture quality. If it backfires it is one
  message and trivially reverted.

---

## Done ✓

- [x] xHome region fix (403 error)
- [x] 4 lag investigation toggles (REMB interval, TWCC delay injection, ramp, periodic keyframe)
- [x] Aggressive flush thresholds (75ms/300ms/3s/10 drops)
- [x] SDP H264 constraints (profile-level-id preserved, max-fr, max-fs=8160)
- [x] GAME profile: 2500kbps, 1ms sleep, queue=1
- [x] RTCP RR/SR parsing (loss%, jitter, NTP timestamp)
- [x] Dynamic REMB adjustment (loss/jitter/lag-based + recovery)
- [x] Periodic keyframe PLI (200ms toggle)
- [x] TWCC delay injection with ramp
- [x] Version/build info in settings

---

## Testing Notes

- **Current version**: 0.2.5 (branch `0.2.5`, based on upstream 0.2.4 / `568cca3`)
- **Profiles**: GAME (default ON for low latency), MEDIA (quality), Custom (all toggles)
- **HUD metrics to watch**: `lag`, `d/p`, `ageMax`, `flushes`, `hwb`, `hs:`/`in:` handshake state
- **Test config for the two clean sessions (2026-10-12)**: Custom mode, 3000 kbps cap, Baseline, queue=1, sleep=1 ms, and **all aggressive feedback toggles OFF** (TWCC delay injection, periodic keyframe)
- **Metrics log**: every streaming session writes `ux0:data/xcloud-rust/metrics.csv` (44 columns, 1 Hz, truncated at session start). Always on - independent of the debug overlay and of `pure_stream_mode`:
  ```sh
  curl -s 'ftp://192.168.0.67:1337/ux0:/data/xcloud-rust/metrics.csv' -o metrics.csv
  python3 scripts/analyze_metrics.py metrics.csv
  ```
  Fetch before starting a new stream (a new session truncates the file).
- **Historical aggressive test config**: Custom mode, 2500 kbps, Baseline, queue=1, sleep=1ms, REMB=100ms, TWCC delay injection=ON