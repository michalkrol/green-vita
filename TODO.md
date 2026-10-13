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

### 2. Dark Scene Blockiness (Baseline Profile Limitation) — RESOLVED
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
- **Status**: Fixed in code. First session on this build is the hardware test.
  Watch `decode_errors` and `enc_width`/`enc_height` in the metrics CSV to
  verify Vita HW decoder handles Main (CABAC) without errors.
- **Note**: Console does NOT support High (no High PT in Chrome answer).
  Main is the ceiling. High entries (`64001f`/`640020`) kept as-is for parity
  with Chrome's offer but will not be negotiated.

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