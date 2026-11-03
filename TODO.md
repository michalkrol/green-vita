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
