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
  - `maybe_flush_backlog` never cleared `lag_high_since` when not eligible, so a
    single eligible instant permanently satisfied `LAG_FLUSH_SUSTAIN`.
  - `rtp::flush_expired` was an empty stub; now a stall-based expiry (100 ms from
    the LAST packet, marker-absent only). **0 firings in 49 min measured -
    effectively inert at current rates.**
  - Per-second metrics CSV + `scripts/analyze_metrics.py` (see Testing Notes).
- **Open defects found while measuring**:
  - `rtcp_rr_loss_fraction` is stored as percent but read back as
    `stored / 100.0` and compared against 5.0/2.0/1.0 => the loss-based REMB
    reduction can never fire.
  - `constrain_h264_codec` appends a second `a=fmtp` line hardcoding
    `profile-level-id=42e01f` after the negotiated one (see item 2), and `max-fr`
    is a constant: `video_fps * 90_000 / video_fps` == 90 000.
  - REMB shock is inert: `remb_shock_until` is never armed.
  - Dead counters never incremented anywhere, so HUD `hold:S` and several CSV
    columns are structurally 0: `rtp_abandons`, `rtp_hold_saved`,
    `rtp_wait_drops`, `rtp_invalid`, `dec_queue_full`.
- **Suspected root cause** (unchanged as a hypothesis): console-side send buffer
  buildup. Note `lag` is structurally blind to it; `ageMax`/`display_pickup` are
  client-side and were also flat.
- **Next steps**:
  - 30+ min game session with continuous input (the untested variable).
  - A/B the aggressive toggles against the same workload.
  - If drift reproduces, add a better drift detector (`ageMax`/`pick` or the
    metadata round-trip) since `lag` cannot see the pre-stamp queue.

### 2. Dark Scene Blockiness (Baseline Profile Limitation)
- **Issue**: Dark areas with subtle gradients show heavy blocking/banding
- **Root cause**: Baseline lacks CABAC entropy coding
- **Options**:
  - Test Main profile (if Vita HW decoder supports CABAC)
  - Add SDP `max-qp` constraint (e.g., `max-qp=35`)
  - Add SDP `min-qp` constraint to prevent over-quantization
- **Blocked**: the SDP constraint pass appends a second `a=fmtp` line for the
  video payload type that hardcodes `profile-level-id=42e01f` (Constrained
  Baseline) after the negotiated one, with two fmtp lines for a single payload
  type being invalid SDP and the injected one likely winning. So selecting Main
  or High is probably silently overridden back to Baseline - fix that first or
  "Test Main profile" is a no-op. Main/High also require decode queue depth 3+
  (B-frame reordering) and are unverified on the Vita's HW decoder. Note
  Main/High add reordering latency, which trades against the item 1 goal.

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

---

## Done ✓

- [x] xHome region fix (403 error)
- [x] 4 lag investigation toggles (REMB interval, TWCC delay injection, ramp, REMB shock, periodic keyframe)
- [x] Aggressive flush thresholds (75ms/300ms/3s/10 drops)
- [x] SDP H264 constraints (profile-level-id=42e01f, max-fr, max-fs=8160)
- [x] GAME profile: 2500kbps, 1ms sleep, queue=1
- [x] RTCP RR/SR parsing (loss%, jitter, NTP timestamp)
- [x] Dynamic REMB adjustment (loss/jitter/lag-based + recovery)
- [x] REMB shock (emergency 100kbps burst)
- [x] Periodic keyframe PLI (200ms toggle)
- [x] TWCC delay injection with ramp
- [x] Version/build info in settings

---

## Testing Notes

- **Current version**: 0.2.5 (branch `0.2.5`, based on upstream 0.2.4 / `568cca3`)
- **Profiles**: GAME (default ON for low latency), MEDIA (quality), Custom (all toggles)
- **HUD metrics to watch**: `lag`, `d/p`, `ageMax`, `flushes`, `hwb`, `hs:`/`in:` handshake state
- **Test config for the two clean sessions (2026-10-12)**: Custom mode, 3000 kbps cap, Baseline, queue=1, sleep=1 ms, and **all aggressive feedback toggles OFF** (REMB shock, TWCC delay injection, periodic keyframe)
- **Metrics log**: every streaming session writes `ux0:data/xcloud-rust/metrics.csv` (44 columns, 1 Hz, truncated at session start). Always on - independent of the debug overlay and of `pure_stream_mode`:
  ```sh
  curl -s 'ftp://192.168.0.67:1337/ux0:/data/xcloud-rust/metrics.csv' -o metrics.csv
  python3 scripts/analyze_metrics.py metrics.csv
  ```
  Fetch before starting a new stream (a new session truncates the file).
- **Historical aggressive test config**: Custom mode, 2500 kbps, Baseline, queue=1, sleep=1ms, REMB=100ms, TWCC delay injection=ON, REMB shock=ON