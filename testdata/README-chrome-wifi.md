# rtpdump-chrome-wifi.bin — real capture fixture (Chrome xHome session)

**Source**: `chrome-dumps/20261030-rtcp-chrome-wifi.pcap` (Mac Wi-Fi
interface, 8 m 22 s, 2026-10-30 23:38–23:46 — Chrome running the xHome
client on the same console/network as the Vita app).

**Format**: `RTPDUMP1` — records `[t_us u64][seq u16][ts u32][marker u8]
[len u16][payload]` (little-endian), t_us = µs since first packet
(≈ capture arrival time at the Mac). Video = RTP PT 116 dynamic,
1280×720 @ ~60 fps.

**Contents (full `rtpdump-chrome-wifi.bin`, 166 MB, 157 341 records,
501.8 s)**: video packets with REAL arrival timing. ZERO packet loss,
zero gaps (seq 31783→58051 contiguous) — Chrome's reception was
lossless; its 4 freezes / 50 dropped frames were its own pipeline.

**Key finding**: the console's delivery to any client is bursty but this
capture shows the air delivery itself can be lossless — the Vita's
damage rate on the same nights (dam 0.25–1.3/s) is attributable to the
VITA's own air reception (2.4 GHz-only radio, power-save, position) and
the stream's own airtime saturation at high caps — NOT to neighbors
(all-devices-off test: unchanged) and NOT inherent to the console.

**Trimmed fixture** (`-180s.bin`, 46 MB, 45 436 records): the first
180 s — used by the offline config-matrix replay harness (Fast/Smooth ×
damage policies against real arrival timing; synthetic loss injection
for damage-rate scenarios).

**Regenerate**: see `chrome-dumps/` — the extraction script pattern
is documented in the git history of this README.
