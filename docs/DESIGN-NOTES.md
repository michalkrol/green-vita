# Design Notes — PS Vita xHome Streaming Client (stage 8 redo)

**Source**: Synthesized from `HANDOFF-STAGE8-REDO.md` (entire), `TODO.md:5` (glossary), `TODO.md:6` (mini debug), and code review of `src/streaming/video/metrics.*`, `src/pipeline/media/assembler.rs`, `src/settings.rs`.

---

## 1. Two-Queue Model

**Console pacer queue (Queue A)** — fills when the console is told to send slower than content demands. Primary mechanisms:
- REMB = 15 Mbps constant (the invariant) tells the console its rate; console selects its own ~1.9–3.7 Mbps.
- `b=AS` cap in SDP is effectively ignored by the console; REMB is the real rate knob.
- Menu/pause drains Queue A to floor: lobby lag 1–3 ms with Queue A empty. PROVEN: the console pacer queue is the latency reservoir; freezes release as catch-up bursts to the accumulated point, not floor.

**AP/link queue (Queue B)** — fills when send rate exceeds link capacity. Primary mechanisms:
- REMB-15M era or uncapped re-probe → loss signatures (`dam_first_slice`, `dam_holes`, `queue_full`).
- 36 ms TcD (transport clock delay) tail from radio burstiness.
- `queue_full` 1.30/s = admission-policy spike (S5 arc §6.9).

**Asymmetry lesson**: under-announcing REMB is as harmful as over-announcing. Three "correct-but-harmful" incidents rhyme: TWCC-100 (catastrophic regression, GONE after S5), REMB-15M (15 Mbps const → console over-probes → accumulating latency), REMB-descent (ladder fired on ordinary radio bursts → ratcheted ceiling down with no re-growth). Verdict: static user cap + console's own GCC on TWCC-60 is the plateau.

**Time-delayed degradation has THREE proven mechanisms** (HANDOFF-STAGE8-REDO.md S5 ARC §4):
1. **Feedback poisoning** — thresholds inside healthy noise (NACK 0.5–1.5/s band) arm on the first bursty window coinciding with a content re-probe (race-after-lobby = collision zone). Good start = controller not yet fired.
2. **Console re-probe ramp** — flattering steady state (RR ≈0, TWCC med 42 µs) lets minutes-long climbs outrun the link (killed at root by b=AS cap).
3. **Radio lottery** — external interference alone reproduces the pattern (room experiment: neighbour 2.4/5G devices).

---

## 2. Three Time-Delayed Degradation Mechanisms

These are the three mechanisms that cause quality degradation over time, each with a different time constant and trigger:

| Mechanism | Time Constant | Trigger | Interaction |
|---|---|---|---|
| Feedback poisoning | ~seconds | First bursty window after lobby, coinciding with content re-probe | Arms on healthy NACK noise (0.5–1.5/s); undoes REMB cap benefit |
| Console re-probe ramp | ~minutes | Sustained link usage without adequate feedback damping | Console's own GCC (gradient feedback control) climbs rate; kills at root by b=AS cap |
| Radio lottery | session-variance | External 2.4 GHz interference (neighbour devices, channel overlap) | Room experiment: same scene, same cap, different neighbour activity → dam 1.0–1.4 vs 0.00–0.07 |

**Key insight**: On a clean link, REMB-off + TWCC-60 is best (dam 0.12/s flat). On a radio-interfered link, static cap matters less than radio state. The three mechanisms rhyme: under/over-announcing REMB both cause degradation; the only stable point is static cap + console's own pacing.

---

## 3. AU-Depth Dial (Quality ↔ Floor Exchange Rate)

| Config | AU_QUEUE_MAX_DEPTH | ADAPTIVE_AU_LIMIT | AU_QUEUE_MAX_AGE_MS | Quality Floor |
|---|---|---|---|---|
| **0.3.1** (origin/archive) | **12** | `true` (60 fps → 12) | **0** (depth-reject only) | **~300 ms** = "perfect quality at ~300 ms floor" |
| **stage8** (current) | **≈0–1** | `true` (60 fps → 1, or adaptive) | **0** | **≈213 ms** camera cell (lobby scene, n≥3) |
| **Chrome** | adaptive | adaptive | adaptive | **60–70 ms** self-report (jitter 7–12) same console/Wi-Fi |

**DIAL INSIGHT**: AU-queue depth is the quality↔floor exchange rate. Depth 12 = quality-first (300 ms floor, ~12×16.7 µs ≈ 200 ms bank + ~100 ms legs). Depth 0–1 = latency-first (213 ms floor). Nobody has tried the middle (depth 4–8).

**Worker.rs paced sleep**: `DECODE_PACE_SLEEP_MS=13` (default, restored 2026-10-24/25) + adaptive limit keeps pipeline age ≈2–4 ms. Without sleep, age slopes were +80..400 ms/min (unbounded accumulation). With sleep, age is FLAT (255–280 ms over 15 min vs +80..400 ms/min without).

---

## 4. Grace v2.x Architecture

**Grace v1 (INERT)**: held at the rule-2 flush boundary only. Telemetry: 0 events in 773 s. Real damage resolved via (1) H3 push-immediate (markerless, correctly excluded) and (2) push FORCE-OUT (dominant — next frame arrives ~16 ms after marker, before pump's flush_due).

**Grace v2 (PARKED-AU)**: force-out + rule-2 park a holed NACKed marker-seen AU (1 slot, ≤40 ms). Three resolve paths:
- **grace_filled**: held AU resolved Complete via RTX fill (holes closed, marker landed). Counts Complete outcomes only.
- **grace_expired**: held AU resolved Damaged via expiry (deadline passed, no RTX landed). Identical damaged resolve to synchronous path.
- **grace_held**: AU currently parked (in grace_parked slot).

**Grace v2.1** (18f44f0): `had_first_slice` flip via RTX start packet → continuation-opened AU decodable. Leading-loss door: contiguous-but-unverified marker AUs NACK the leading range once (prev_close+1..first-1, capped at 32) and PARK. `prev_close_before_current` snapshot (last_closed_seq clobbered at AU start); parked span checks vs expected_first.

**Grace v2.2** (leading-loss door): marker-seen AU whose FIRST slice was lost looks contiguous to `compute_holes` (span starts at first present seq) but `had_first_slice == false` proves leading packets missing. Resolves Damaged at marker instantly — never NACKed, never repairable. Instead: NACK the leading range once (prev_close+1..first present−1, capped at 32) and PARK for RTX. Expiry resolves Damaged identically to today.

**Grace v2.3** (06af381): markerless tail rescue. force-out parks markerless holed AUs + NACKs intra-span holes (queueing previously only fired at marker → RTX never attempted). `try_fill` carries marker bit (RTX marker sets marker_seen); accepts marker packet adjacent above span; completion requires holes==0 AND marker_seen (markerless fills wait parked). `grace_filled` counts Complete only.

**Grace budget**: `GRACE_BUDGET_US = 40_000` (≈ one radio tail / TWCC median tail 36–39 ms). Gives S5 RTX repair time before damaged resolve fallback.

**Grace hold statistics** (fb97f6a, 409 s cap5m busy):
- held=23 filled=23 expired=0 → **100 % fill rate, zero expiries**
- dam_first_slice 0.341/s vs 3.263 (v1-busy) / 0.819 (quiet)
- dam_holes 0.636 → 0.005 = mid-frame holes ELIMINATED (grace signature)

**Grace v2.1+v2.2 (18f44f0, gates 169/22)**:
- (a) parked-fill upgrades had_first_slice/is_idr/nal_ref_idc (RTX start packet flips a continuation-opened AU decodable)
- (b) leading-loss door — contiguous-but-unverified marker AUs NACK leading range once (prev_close+1..first-1, cap 32) and PARK; `prev_close_before_current` snapshot; parked span checks vs expected_first; H3-shape legacy tests → grace-off ctor.

**Residual door (v2 LIVE verdict)**: dam_holes≈0 but dam_first_slice 0.341 ⇒ first-slice loss. Leading-lost AUs look contiguous to compute_holes (span starts at first present seq) and resolved Damaged at marker instantly, never NACKed.

---

## 5. Micro-JB Boost Layer (Step 5)

**Primary target**: queue_full 12/s bursts + freeze-catch-up visibility. NOT the 697 ms upstream pacer queue (client can't fix; only cap choice does).

**How it works** (33fc7de, flag MICRO_JB=false, gates 178/22):
- stage8 ALREADY had a 6-deep adaptive bank (fps-derived admission + paced yield — the S0 Paced arm, active).
- The missing piece was event-driven adaptivity:
  - `scheduler.admit` gains `jb_boost` (slots on top of the fps limit, clamped JB_BOOST_MAX=2 ≈ +33 ms worst case).
  - `worker.submit` swaps `MICRO_JB_EVENTS` (static fed by assembler grace parks — burst signature) into boost.
  - Decays after 2 s quiet; flag off ⇒ boost 0 ⇒ byte-identical known-good path.

**Latency contract**:
- Clean = zero burst effect.
- Burst = +16–33 ms transient on affected frames (vs artefact/stutter).
- Race start untouched by design (growth is damage-event-driven, not content-driven).
- 697 ms upstream pacer queue out of scope (client can't fix; only cap choice does).

**CSV columns added**: `jb_boost`, `jb_depth`, `jb_absorbed` (pushing total from 87 to 96, then 97 with `damaged_dropped`).

**A/B verdict** (61adbc0, 338 s ON session vs OFF baseline):
- ON: queue_full 0.53/s vs 1.09/s = **4× less shedding per unit NACK**.
- presented 52.9 stable, lag 9.5 ms.
- grace 86/73/13 (85 %).
- jb_boost max 2, jb_depth max 6, jb_absorbed 5830 (coarse: counts all submits under active boost).

---

## 6. Damage Policies (LowLatency/Balanced/Quality)

**Mechanism correction** (8d4648c): damaged AUs were ALREADY never decoded (Damaged arm drops them, PLI requested). Visible artefacts are the DECODER'S CONCEALMENT on the FOLLOWING frames (macroblocking) until PLI→IDR reset. The setting chooses the per-damaged-frame policy at the RTP Damaged arm:

| Policy | Behaviour | When Used |
|---|---|---|
| **LowLatency** (default) | Decode-through: decoder conceals on next frames (brief artefacts, no stall). | Fast games; latency-sensitive. |
| **Balanced** | Shed-to-IDR on severe damage only: `!had_first_slice || holes > 1`. Worst concealment avoided. | General use; artefact↔freeze trade-off. |
| **Quality** | Shed-to-IDR ALWAYS on every damaged frame — artefact-free, accepts micro-freeze per event. | Slow/story games; artefact-averse. |

**Measured 240 ms PLI→IDR floor**: The original accumulation claim (~10–20 ms/min glass-to-glass) is SUPERSEDED (not reproducible client-side in s1–s4). The live question is now drops/queue_full per LATENCY-RECOVERY-PLAN.md.

**Quality-mode racing verdict** (8d4648c, 326 s cap5m): USER VERDICT "good enough for racing game" — latency NOT worse than LowLatency, quality typically better. Data: dam 0.080/s (26 events, calm NACK 0.29/s), Quality sheds 31 (max 2/s), resyncs 0.095/s, presented 53.5 with NO stall dips, lag thirds 15.6/3.0/5.0 ms, grace 10/10/0 (100 %), jb absorbed 1043.

**damaged_dropped CSV column** (Step 6, 8d4648c): Damaged AUs dropped+PLI requested. Mirrors the per-frame policy choice.

---

## 7. Cap Advisor Heuristics + the Constant-36ms Open Question

**Probe at sign-in**: gateway TCP-connect RTT + ≥64 KB GET read-back (3 s budget). `suggest_kbps` ladder:
- Downlink-trusted (≥64 KB) ladder: 5000/2500/1500/1000 kbps.
- RTT ladder: ≥15 ms → 2500; ≥40 ms → 1500; else <5 ms → 5000; <15 ms → 5000; <40 ms → 2500.

**Constant-36 ms question**: The gateway TCP-connect RTT reads a constant ~36 ms on EVERY probe (10/10, advisor v1.1 era). Suspicious: the Mac on the SAME 2.4 GHz channel measures gateway RTT median 2 ms; the Vita's UDP stream path runs at floor latency. So ~36 ms is Vita-TCP-path quantization (Wi-Fi power-save buffering the SYN handshake?), not channel truth.

**Interim effect**: v1.1/v1.2 thresholds (RTT ≥15 ms → ≤2500, ≥40 ms → 1500) mean a constant 36 ms forces a low suggestion at EVERY sign-in — the advisor is effectively blind until resolved.

**Provenance columns** (CSV: `probe_rtt_ms_x10`, `probe_dl_mbps_x10`, `probe_suggest_kbps`): Constant per session; 0 = no probe ran. Set at stream start, survive reset.

**User-approved resolution** (d9b4dd8): probe fires at TitleList/ConsoleList entry (post-auth), RTT = median of 3 connects; downlink split to its own helper. v1.2 VALIDATED: post-auth RTT=6.0 ms (was 35.5 during-auth) → suggest 5000 → user played 5M, "very smooth, one minor slowdown at start, low artefacts".

**Best known config**: TWCC-60 + RR + static b=AS cap, REMB off. REMB descent-only is HARMFUL on this link — the ladder fired on ORDINARY radio bursts (NACK ≈0.9/s) and ratcheted the ceiling down with no re-growth.

---

## 8. Floor Measurements

**Lobby scene, camera n≥3 (not latency_test_mode)**:
- Camera cell: **178–213 ms** (HANDOFF-STAGE8-REDO §4: median 210 ms, n=5 = 199/202/210/240/240 → median 210 ms, spread 41).
- HUD `lag`/`lagMax` in lobby: **0 ms static** (no drift). User eyeball ↔ CSV correlation confirms.
- Chrome debug overlay self-reports **60–70 ms (jitter 7–12)** same console/Wi-Fi.

**Why the 210 ms floor lives in capture/encode/input/display legs**: Chrome 60–70 ms self-report; Vita 213 ms camera cell. The difference lives in the capture/encode/input legs, not the client-side pipeline. The client is bounded by the console's own capture→pacer stages (measured ≈0–32 ms from video-timing extension, 0.2.4-drift era).

**No accumulation**: age slopes ~0 or negative in sessions s1–s4. The "10–20 ms/min glass-to-glass accumulation" claim is superseded. The live question is drops/queue_full (backpressure suspect).

---

## 9. A/B Protocol (One Variable Per Session)

**Protocol rules** (HANDOFF-STAGE8-REDO.md §4, Process rules):
- **One variable per device build**; ask before flash; subagents only for pure modules/fixtures.
- **No checkout/reset/stash**; never overwrite AGENTS.md/HANDOFF.md.
- **Atomic order-following tasks only** (no architecture/design/heavy thinking).
- **Explicit NO-DELETION bans** — subagents have deleted entire directories incl. git repos (machine AND Vita). Confine to one named file.
- **Independently re-run the gate and check byte counts before believing any success claim**.

**Flash+md5 archive naming**: `metrics-archive/metrics-<HEAD-md5>-<label>-<seconds>s-<date>.csv`. Example: `metrics-3bfcd9b-s0-hudpobe-248s-20261101.csv`. Metadata in CSV footer: `// session_duration_s=<n>, total_rows=<m>, dropped_rows=<k>`.

**A/B gates**: 152 passed / 22 warnings (host-test.sh with `set -o pipefail`). Gates verify: md5 download-back, eboot integrity, CSV column count match.

**A/B example naming patterns**:
- `metrics-ce515c5-main-nocap-194s-20261101.csv` (REMB-off, no cap)
- `metrics-ce515c5-cap5m-546s-20261101.csv` (static 5 Mbps cap)
- `metrics-2275aa8-rembon-cap5m-288s-20261101.csv` (REMB-on + 5M cap, then reverted)
- `metrics-b4f27d5-twcc60-badlink2-348s-20261101.csv` (TWCC-60 bad link)
- `metrics-cd1b1ca-remboff-badlink-545s-20261101.csv` (REMB-off + TWCC-60 best through-walls)

**One variable per session**: e.g., flip MICRO_JB true/false; flip SEND_REMB true/false; change b=AS cap from 1500 to 5000; enable/disable TWCC-60. Never combine variables in one session.