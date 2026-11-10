# TODO

## Queue added 2026-11-01 (ordered)

1. **fps wiring fix** — `api_xbox/streaming/rtc/worker.rs:51` hardcodes
   `request_video_fps(&sdp, 30)`; thread `unlock_video_fps` so the settings
   switch actually reaches the offer (60 path exists + unit-tested, never
   called). Signaling-honesty: console reportedly ignores a=framerate anyway.
2. **Safe-Ceiling REMB** (behind `SEND_REMB=false` gate): value source =
   `settings.video_bitrate_kbps` read FRESH per send (mid-session changes
   follow live); descent-only from LIVE pre-repair counters
   (`rtcp_nack_sent`/`dam_holes` — NOT `rtp_gaps`, frozen/dead); passive
   net, never above user cap, no active growth. Loss proxy = NACK/s ladder
   pending user/eng confirm.
3. **RGBA8888 color profile restore** (history: `5076be8`, ColorProfile
   `3d3e075`/`382ea3a`) — kills 565 banding ("reads as blockiness").
   Caveat: hardware finding says +latency (32-bit decode output doubles
   per-frame memory bandwidth, LATENCY-FINDINGS 2026-10-23) → keep Rgb565
   default, device A/B before adopting.
   ✅ IMPLEMENTED (0.3.2): Settings → "Color profile" row (RGB565 default /
   RGBA8888, i18n'd), threads settings → DecoderConfig → sceAvcdec
   pixelType + SDL texture format; applies at next stream start. Clarifies
   the old worry: the H.264 bitstream (and thus Wi-Fi throughput demand) is
   UNCHANGED — decode output format is purely local; the real cost is local
   CDRAM bandwidth only. Device A/B still pending (compare `d/a`, `hwb`,
   sheds vs a Rgb565 session in a dark-scene title).
4. **RGB565 dark-color dither** — arch §6.7 owns it: ordered dither at the
   conversion point (S1 GPU shader; fallback "RGB565 decode + GPU dither").
   Feasible WITH S1; standalone post-hoc CPU dither is weak (cannot
   un-quantize already-banded data). User's original idea, canonized.
5. **Document recent architecture + ALL debugging parameters** — glossary
   for every HUD field (d/p, d/a, skip, cap, repl, q, hwb, ageMax, lag,
   lagMax, bw, aq, slip, slipS, qS, cta, ctaMax, srAge, srCoff, TcD,
   TcDMx, r:, rs, rst, wk, ab, hold, iv, qf, fl, tw, stride, md, pick,
   pickMax) and all 87 CSV columns + settings flags, so any reader can
   interpret sessions unaided.
6. **Mini debug mode** — bitrate/bandwidth + displayed FPS only, white text
   on semi-transparent dark rectangle, TOP-RIGHT corner. Settings "debug"
   control becomes a rotating option (Off / FPS+Bitrate / Full debug) like
   the profile/bitrate rows. Display metric: **bandwidth** (our only live
   number is measured `video_bandwidth_kbps`; "bitrate" implies encode-side
   which we do not measure) shown in Mbps.
7. **Network validation & safety (vitasdk), two phases:**
   - Phase 1 pre-sign-in: `sceNetCtlInetGetInfo` Wi-Fi state check BEFORE
     the automatic sign-in; offline → clean error UI ("No Wi-Fi connection
     detected…"), retry/exit, no sign-in attempt, no crash.
   - Phase 2 parallel with sign-in: gateway via
     `SCE_NETCTL_INFO_GET_GATEWAY` → non-blocking TCP handshake to gateway:80
     (RTT + loss/timeout from 2.4 GHz interference) → lightweight
     `GET / HTTP/1.1` read-back (downlink) → metrics to UI warning banner
     when below thresholds; must not block sign-in.
9. (DEMOTED 2026-11-02, user: next step = floor latency) **Cap-advisor RTT mechanism check** — the gateway TCP-connect RTT reads
   a constant ~36 ms on EVERY probe (10/10, advisor v1.1 era). Suspicious:
   the Mac on the SAME 2.4 GHz channel measures gateway RTT median 2 ms
   (netmonitor/), and the Vita's UDP stream path runs at floor latency —
   so ~36 ms is Vita-TCP-path quantization (Wi-Fi power-save buffering the
   SYN handshake?) rather than channel truth. Interim effect: v1.1/v1.2
   thresholds (RTT ≥15 ms ⇒ ≤2500, ≥40 ms ⇒ 1500) mean a constant 36 ms
   forces a low suggestion at EVERY sign-in — the advisor is effectively
   blind until this is resolved. Check plan: flash d9b4dd8 (median-of-3,
   post-auth) and log all 3 components; try a UDP probe variant (the
   streaming UDP path is fast, so UDP RTT sidesteps TCP/power-save);
   correlate with netmonitor gateway pings; consider scePower
   wireless-mode toggles scoped to the probe only (knobs exist in
   shell/power.rs, ENABLED=false for drift reasons).
   NEW DATAPOINT (2026-11-02, user): the ~36 ms is NOT universal — it is
   STABLE WITHIN one app run and RESETS on the next app start. Strengthens
   the per-association hypothesis: the Vita's Wi-Fi power-save/connection
   mode is negotiated at association/first-use and quantizes TCP RTT for
   that whole session. The Mac on the same channel still reads 2 ms, and
   the streaming UDP path runs at floor latency — so it is TCP/handshake
   -path specific. Advisor impact: within a run the suggestion is at
   least CONSISTENT (same input → same output); only across-run changes
   shift it.

## Roadmap 0.3.2 → 0.3.3 (added 2026-11-03, user-directed)

10. **Xbox home button / in-stream menu redesign (BRAINSTORM).** Current:
    Xbox button → pause overlay [Resume, Settings, PressGuide, ExitGame].
    User finds the settings depth unintuitive. OS CONSTRAINTS (verified):
    the Vita's physical PS button is owned by the OS (LiveArea) — userland
    apps cannot hook it, so a "PS-home opens our menu" model is impossible;
    a crashed app AUTO-returns to LiveArea (OS kills the process — no
    zombie), and third-party apps CANNOT inject entries into the system
    settings menu (needs system privileges). Ecosystem comparison: Moonlight
    = guide long-press → in-app menu, short → passthrough (we already do
    both via PressGuideButton); RetroArch = hotkey combos; PS Remote Play
    on Vita = PS button → OS overlay. PROPOSALS: (a) add "Quit app (to
    LiveArea)" entry to the overlay — covers intentional-close without
    menu navigation; (b) quick-toggle chips INSIDE the overlay (damage
    policy, bitrate preset, debug mode — no separate settings page depth);
    (c) long-press Xbox button = overlay, short = passthrough (explicit
    distinction, Moonlight-style); (d) crashes are OS-handled — document,
    don't engineer. DECISION NEEDED: which proposals to adopt.
11. **Mid-game video settings (feasibility matrix).** Bitrate cap: ✗ NOT
    possible mid-session — b=AS is SDP-time; the only mid-session rate
    signal is REMB (verdicted harmful). H264 profile: ✗ NOT possible —
    fmtp is SDP-time. Buffer/jb params (target/boost/decay): ✓ runtime —
    promote consts to settings, store to statics the worker reads per
    submit. Damage policy: ✓ ALREADY runtime-capable — DAMAGE_POLICY
    static is read per-event; wire settings changes to re-store live
    (~5-line change, highest value per effort).
12. **RGBA8888 + H264 High profile.** RGBA8888 = item 3 above (queue
    history: 5076be8/3d3e075 killed 565 banding; caveat: 32-bit decode
    output doubles per-frame memory bandwidth — device A/B required).
    H264 High: add enum variant to H264Profile + fmtp profile-level-id
    handling; CAVEAT: Vita decoder compatibility unproven for High
    (Main verified working). External context: new PSVita WiFi driver
    (+20–40 % connectivity) and Series X faster encode are EXTERNAL
    improvements — they benefit every client; our stack already exposes
    the knobs (caps, policies) to exploit them.
13. **i18n audit findings (2026-11-03).** Hardcoded English labels:
    "H264 profile: {}", "Damage policy: {}", "Debug: {}" (format! in
    settings.rs), damage-policy value labels (Low latency/Balanced/
    Quality), debug value labels (Off/FPS+Bitrate/Full), "Exit app" (when
    added, item 10). Locale coverage: net-cap-* keys en-US/en-GB only
    (I18n falls back to en-US). Fix plan: ftl keys for all format!
    labels + value enums; propagate the 4 newest key groups (net-cap-*,
    error-no-wifi, damage/debug labels) to all 19 locales.
14. **Gamepad mappings config page (0.3.3, BRAINSTORM).** GameProfile
    already carries per-title toggles — extend with a button→button
    mapping table (persisted, per-title, UI page with capture-a-press
    flow: "press the physical button, then the target"). Combos/macros:
    record a press sequence with inter-press timing, replay on a trigger
    button (bound per profile). Risks: input loop ordering (SDL events vs
    the mapping layer), combo firing rate vs the 4 ms input floor.
    Feasibility: high — everything flows through GamepadFrame already.
15. **Latency profile setting (Fast / Smooth) — the 0.3.1 quality
    recreation, accumulation-safe (2026-11-03, user-approved direction).**
    Promote micro-jb consts to a settings-driven config: Fast = current
    (target 0–1, boost ≤2, +0 ms floor); Smooth = bank target ~6, hard
    cap 12 (≈ +100–200 ms floor), paced drain — bursts and PLI→IDR shed
    stalls absorbed invisibly. ACCUMULATION SAFETY: the bank is
    hard-capped in admission (bounded latency by construction); TWCC
    arrival stamps pre-bank (console feedback undistorted); stream_lag +
    jb_depth monitor drift per-second. Interplay: Smooth + Quality
    damage policy = the full 0.3.1 recreation (bank hides the 240 ms
    PLI→IDR shed stalls — measured) with the repair stack 0.3.1 lacked.
    Per-title pairing with DamagePolicy = the per-game control.
    IMPLEMENTATION: MicroJbConfig from Settings (promote consts), add
    target-depth hold to the worker admission/drain, extend jb telemetry
    (target + held counts). Effort: small-medium — admission param,
    paced drain, and telemetry all already landed.
16. **Error handling & robustness pass (2026-11-03, user report).**
    (a) **Xbox closes the connection → app hangs or crashes.** Repro:
    end/close the stream from the console side. Suspects: the teardown
    path (poll loops, channel sends, worker Drop ordering) blocking or
    unwrapping on an expected message. Fix: graceful connection-closed
    handling (no unwrap/expect on teardown; bounded joins; clean exit to
    pause/menu with a "Stream ended by console" message).
    ✅ PARTIAL (0.3.2): Closed now shows a localized "Stream ended by the
    console" screen (was a silent bounce to the list); teardown
    breadcrumbs (eprintln + elapsed ms) at stop_stream_with_error /
    exit_stream / backend DELETE / RtcWorker drop / worker-loop exits pin
    the blocking stage on the next repro; send_important_event is bounded
    (2 s try_send backoff, then drop) so a full events channel can no
    longer wedge the worker thread during app-side teardown. The actual
    hang needs the repro to pin — read the breadcrumbs from the next one.
    (b) **Wi-Fi off → crash on app turn-on.** ROOT CAUSE FOUND
    (2026-11-03): the Wi-Fi gate fails OPEN on sceNetCtl syscall errors
    (Unknown ⇒ allow sign-in) — an uninitialized network stack then
    crashes downstream. FIX DESIGNED (one line, intentionally deferred):
    fail-CLOSED (Unknown ⇒ offline screen) + update the gate's unit
    test. VERIFY on-device with the radio fully off after applying.
    ✅ APPLIED (0.3.2): blocks_sign_in now `state != Online` (fail-closed,
    Unknown ⇒ offline screen); test `only_online_allows_sign_in`.
    **DEVICE VERIFY PENDING**: radio fully off → app start must show the
    offline screen, not crash.
    (c) **Unintuitive error messages** (raw MS server/auth/HTTP errors
    surfaced verbatim). Fix: an error-classification layer — map known
    error classes (HTTP status/auth codes/XSTS errors/network failures)
    to friendly localized messages per source (sign-in, auth,
    console-list, stream), keep the raw detail in `details` for
    diagnosis. TODO-13 i18n rules apply (ftl keys, en-US + fallback).
    ✅ IMPLEMENTED (0.3.2): `app/error_classify.rs` — substring classifier
    (HTTP status lines, auth/XSTS, rate-limit, server, timeout, DNS,
    network transport) wired into the ONE choke point `App::localized_error`
    so every error screen benefits; raw text stays in `details`; digit-
    boundary guard so session ids with 401/429 don't misclassify; unit
    tests incl. "connection closed by the console" must NOT classify as
    network. 7 new `error-class-*`/`error-stream-ended-by-console` keys
    propagated to all 19 locales.
    (d) After (a)+(b): a full robustness sweep of every `unwrap`/`expect`
    on session/network paths. — still open (deferred by design; census so
    far: session paths are clean, SDP parsing holds most of the unwraps).
17. **Idle/screensaver stale-stream handling (2026-11-09, user report).**
    Symptom: leave the Vita idle mid-stream (game paused, console enters its
    screensaver/clock after a few minutes) → the stream goes stale and never
    recovers; the only way out was Exit + manual reconnect.
    INVESTIGATION (why preservation is not possible client-side): neutral
    gamepad frames ALREADY flow every 50 ms while idle
    (GAMEPAD_REFRESH_INTERVAL, worker.rs send_sampled_gamepad_frame) and the
    HTTP keepalive runs every 30 s — the console's idle/screensaver state
    ignores both. Once dark, it stops sending video and (observed) never
    resumes; there is no wake/remote-control API for the screensaver state.
    Faking input to prevent idling was rejected (unwanted game input).
    MECHANISM IMPLEMENTED (0.3.2):
    - Stall detection: no decoded frame for 20 s (STREAM_STALL_THRESHOLD;
      healthy floors send frames even on static scenes) → one guide-button
      wake pulse (press_guide_button) as the preservation attempt.
    - If media resumes: stall clears, auto-opened overlay closes itself.
    - If not: the pause overlay auto-opens focused on a new "Reconnect
      stream" entry (index 1) with a localized amber banner explaining the
      stall — one Confirm press tears down the dead session and restarts
      the SAME console/title through the normal StartingStream flow
      (App::reconnect_stream; target rebuilt from return_target + service
      lists; cleanup_active_sessions makes the server-side stop
      idempotent).
    - rtc worker: RTCPeerConnectionState::Failed now exits the pump loop
      as terminal (Closed already did); Disconnected stays recoverable.
    DEVICE VERIFY PENDING: idle → screensaver → pick up Vita → expect the
    banner + one-button reconnect; also confirm the wake pulse alone
    sometimes recovers (Resume then works).
