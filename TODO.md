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
