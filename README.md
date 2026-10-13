<p align="center">
  <img src="assets/greenvita.svg" alt="GreenVita" width="220">
</p>

<h1 align="center">GreenVita</h1>

<p align="center">
  Xbox Cloud Gaming on PlayStation Vita.
  <br>
  A native Rust client with an egui/SDL2 interface and hardware H.264 decoding.
</p>

<p align="center">
  <img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-f74c00?style=for-the-badge&logo=rust&logoColor=white">
  <img alt="PS Vita homebrew" src="https://img.shields.io/badge/PS%20Vita-homebrew-44aa00?style=for-the-badge">
  <img alt="Xbox Cloud Gaming" src="https://img.shields.io/badge/Xbox-Cloud%20Gaming-107c10?style=for-the-badge&logo=xbox&logoColor=white">
</p>

## Settings guide

### Wi-Fi constraints

The PS Vita's Marvell 88W8787 chip uses 2.4 GHz 802.11n — ~30 Mbps theoretical,
but ~8 Mbps usable in practice. Higher bitrates exceed the radio's airtime
capacity, leading to packet drops, macroblock artifacts, and multi-second
smearing that cannot recover until a full keyframe arrives.

Overclocking CPU/XBAR helps Vita-side packet processing (RTP reassembly,
RTCP generation), but the **dominant bottleneck is WiFi airtime** (~8 Mbps
usable). If your bitrate exceeds airtime, overclocking won't prevent
packet loss.

Fix WiFi environment first (20 MHz channel, clean channel 1/6/11,
Bluetooth off). Overclocking is a minor optimization, not a fix.

- Use 20 MHz channel width on your router (not 40 MHz — causes retransmits)
- Pick an unused channel (1, 6, or 11)
- Turn Bluetooth OFF in Vita system settings (shares the same 2.4 GHz antenna)

### Stream profiles

The Stream profile setting batch-tunes all advanced parameters for known
scenarios. Changing profiles requires a video reconnection.

| Profile | Bandwidth | H264 | Sleep | Queue | HUD | When to use |
|---------|-----------|------|-------|-------|-----|-------------|
| **Game** | 2500 kbps | Baseline | 1 ms | 1 | Hidden | Fast-paced games (racing, FPS) |
| **Media** | 7500 kbps | High | 2 ms | 4 | Visible | Slow-paced/story games |
| **Custom** | User-set | User-set | User-set | User-set | Configurable | Fine-tune every knob |

### Advanced stream config — Custom profile

#### Bandwidth cap
0 = unlimited, 500-8000 kbps. Lower = faster response, fewer artifacts.

#### H264 profile
- **Baseline** (fast decode): no B-frames, works with any queue depth
- **Main** (balanced): B-frames, requires queue depth 3+
- **High** (image quality): B-frames + CABAC, requires queue depth 3+

#### Decode sleep
- 0 ms = fastest decode, may cause artifacts on noisy WiFi
- 1 ms = safe default for most setups
- 2 ms = safer on noisy/weak WiFi

#### Decode queue depth
1 frame = ~16ms latency. Higher = smoother on jitter but more latency. Main/High profiles require 3+.

#### Hard bandwidth cap
When ON, enforces the bandwidth ceiling via REMB feedback. When OFF, Xbox manages its own rate (default 15 Mbps).

#### Disable UI overlay
Bypasses egui entirely, saving ~8.3ms display latency (half a frame at 60 FPS).

---

### Lag investigation knobs — Custom profile only

These settings expose the RTCP feedback loop for empirical tuning. They are **only visible in Custom profile**; Game/Media profiles use fixed presets.

| Setting | Range | Default | What it does |
|---------|-------|---------|--------------|
| **REMB interval** | 100 / 250 / 500 / 1000 ms | 500 ms | How often we tell Xbox our max receive bitrate (REMB). Lower = Xbox reacts faster to bandwidth changes. **100 ms** = aggressive (best for unstable WiFi). **500 ms** = conservative (stable WiFi). |
| **TWCC delay injection** | On / Off | Off | When our measured bitrate exceeds your cap + 10%, we report fake "network delays" to Xbox's congestion controller. This triggers its overuse detector to throttle bitrate down. Use when: bitrate overshoots your cap during WiFi contention. |
| **TWCC delay ramp max** | 1–20 | 6 | Strength of the fake delay signal when injection is ON. 1 = barely noticeable, 20 = strong throttle. Higher = Xbox backs off more aggressively. Start at 6, increase if overshoot persists. |
| **Periodic keyframe (PLI)** | On / Off | Off | Sends PLI (Picture Loss Indication) every 200 ms asking Xbox for a fresh keyframe (IDR). **Primary purpose: clears macroblock artifacts / frozen blocks** by forcing a full frame render. Encoder "catchup" is experimental/theory. **Risk:** Xbox may rate-limit or ignore frequent PLI; spamming PLI can overload encoder and worsen backlog. Use when: persistent blocky/frozen areas that don't recover. Cost: extra bandwidth (large I-frames every 200 ms). |
| **RSSI-based bitrate cap** | — | — | *Disabled — NetCtl crashes on some Vita units.* |

---

#### When to tweak each (by observed symptom)

| Symptom you see | Try this first |
|-----------------|----------------|
| **Latency keeps climbing, won't come down** | Lower **REMB interval** to 100 ms; ensure **Hard bandwidth cap** is ON |
| **Bitrate overshoots cap during WiFi contention** | Enable **TWCC delay injection**; increase **ramp max** to 10–20 |
| **Persistent blocky/frozen macroblocks that don't recover** | Enable **Periodic keyframe** (forces full frame refresh) |
| **Sudden quality drop after 30+ min session** | Lower **REMB interval** to 100 ms; ensure **Hard bandwidth cap** is ON |
| **Video stutters on weak WiFi** | Lower **Bandwidth cap** manually; ensure **Hard bandwidth cap** is ON |

---

#### How the feedback loop works (plain English)

```
Vita (you)                              Xbox (stream source)
────────────────────────────────────────────────────────────────
     │                                        │
     │  Video frames arrive                   │
     ├───────────────────────────────────────►│
     │                                        │
     │  "My max bitrate is X" (REMB)          │
     │◄───────────────────────────────────────┤
     │  "Packets arrived at these times" (TWCC)│
     │◄────────────────────────────────────────┤
     │  "Send me a fresh keyframe" (PLI)       │
     │◄────────────────────────────────────────┤
     │                                        │
     ▼                                        ▼
Xbox's Google GCC (congestion controller)   You just report conditions
- "Is network delay increasing?"            - "I got X bytes in Y ms"
- "Should I send slower?"                   - "Loss = Z%"
- Adjusts encoder bitrate                   - "RTT = N ms"
```

**REMB** = "Don't send faster than this."
**TWCC** = "Here's exactly when packets arrived — you figure out the delay."
**PLI** = "Send me a clean frame (IDR), the current one is corrupted."

---

#### Pro / Con summary

| Feature | Pros | Cons |
|---------|------|------|
| **Lower REMB interval** (100 ms) | Faster reaction to bandwidth drops | More RTCP overhead (~1% bandwidth) |
| **TWCC delay injection** | Forces Xbox to pace smoothly on contention | May under-utilize bandwidth if too aggressive |
| **Periodic keyframe** | Clears frozen/blocky artifacts | Extra bandwidth (large I-frames every 200 ms) |

---

### Known limitations (0.2.5)

- **The H.264 profile setting now reaches the console.** The SDP constraint
  pass (commit a6545c1) merges `max-fr`/`max-fs` into the existing `a=fmtp`
  line and preserves the negotiated `profile-level-id`. The duplicate-fmtp
  bug is fixed. Main profile works at decode queue depth 1 (verified 15.2 min,
  `decode_errors 0`), so B-frame reordering is not a blocker; the earlier
  caveat that Main/High need queue depth 3+ was based on an untested concern.
  High profile remains unverified (console refuses it).
- **`lag:`/`lagMax:` are transport-side only.** They measure RTP schedule slip
  and are structurally blind to the console's pre-RTP-stamp queue, so a flat
  `lag:` does **not** mean zero glass latency.
- **The console ignores both `a=framerate` and `max-fr`** and picks its own
  output rate from content and activity. Measured `src_fps` averaged 70.2 for a
  racing game; another session stayed at 58–60 fps with the "Unlock video FPS"
  switch OFF (offer declares `max-fr=30`, `a=framerate:30`). The 30/60 switch
  has no effect on the console's output rate.
- **The console skips whole access units of its own accord.** These appear as
  `drop:` and feed the drop-count flush trigger, while RTCP RR loss stays at 0%
  - they are not packet loss.
- **Some HUD fields are structurally zero** because nothing ever increments
  them: `hold:S` (saves), plus `abandons`, `wait_drops`, `invalid`,
  `dec_queue_full`, and `video_metadata_sent`. The only live member of
  `hold:S/E/G` is `E` (stall expiries), which fired once — at t=2 s of a
  session, during the startup burst — and never again.
- The 0.2.5 latency accumulation and artefacts were caused by the once-per-second
  blocking metrics-CSV write on the RTC pump thread (fixed in 966f885, confirmed
  on hardware at a 4000 kbps cap with low latency, no accumulation, and only
  minor PLI-clearable artefacts). All pre-fix measurements are suspect.

---

### Debug metrics log

Every streaming session writes a 44-column, 1 Hz CSV to
`ux0:data/xcloud-rust/metrics.csv`, truncated at the start of each session. It is
always on - it does not depend on the debug overlay or on `pure_stream_mode`.
Fetch it over the Vita's FTP server and analyse it offline:

```sh
curl -s 'ftp://<vita-ip>:1337/ux0:/data/xcloud-rust/metrics.csv' -o metrics.csv
python3 scripts/analyze_metrics.py metrics.csv
```

The analyser prints a per-minute aggregation table, least-squares drift trends
(ms/min) and a chronological event log for flushes, resyncs, resets and stall
expiries. Fetch **before** starting the next stream, since a new session
truncates the file.

---

## Install

You need a homebrew-enabled PS Vita with VitaShell and an Xbox account that has
access to Xbox Cloud Gaming.

1. Download `green-vita.vpk` from the
   [latest release](https://github.com/michalkrol/green-vita/releases/latest).
2. Transfer the VPK to the Vita.
3. Install it with VitaShell.
4. Launch GreenVita and complete the device-code sign-in.

> [!IMPORTANT]
> Enable **Unsafe Homebrew** in HENkaku Settings. GreenVita needs access to the
> Vita hardware video-decoder module.

## Build

### 1. Install VitaSDK

GreenVita follows the official [VitaSDK/VDPM setup](https://github.com/vitasdk/vdpm).
On Arch Linux, install the host tools first:

```sh
sudo pacman -S --needed base-devel git cmake python wget patch p7zip tar pkgconf rustup
```

Then bootstrap VitaSDK and its port libraries:

```sh
git clone https://github.com/vitasdk/vdpm
cd vdpm
./bootstrap-vitasdk.sh

export VITASDK=/usr/local/vitasdk
export PATH="$VITASDK/bin:$PATH"

./install-all.sh
```

`install-all.sh` installs the Vita port libraries used by this project,
including SDL2 and Opus. The old vitaGL/vitaShaRK dependency list is not needed.

Add these exports to your shell profile so future terminals can find VitaSDK:

```sh
export VITASDK=/usr/local/vitasdk
export PATH="$VITASDK/bin:$PATH"
```

> [!TIP]
> On Windows, VitaSDK recommends following the Linux instructions through
> WSL2. MSYS2 is also supported by VDPM, but WSL2 is the simpler route.

### 2. Install the Rust tools

```sh
rustup toolchain install nightly
cargo +nightly install cargo-vita
```

See the official [`cargo-vita` documentation](https://github.com/vita-rust/cargo-vita)
for its complete command reference.

### 3. Build the VPK

From the GreenVita repository:

```sh
make vpk
```

The Makefile supplies the required Vita Rust flags. The resulting package is:

```text
target/armv7-sony-vita-newlibeabihf/release/green-vita.vpk
```

<details>
<summary>Building without Make</summary>

Unix-like shell:

```sh
RUSTFLAGS="-C target-feature=-neon" cargo +nightly vita build vpk --release
```

Windows PowerShell:

```powershell
$env:RUSTFLAGS = "-C target-feature=-neon"
cargo +nightly vita build vpk --release
```

The repository contains platform wrappers under [`tools/`](tools/) so Cargo can
find the Vita compiler, archiver, and pkg-config implementation on Unix and Windows.

</details>

## Develop On Hardware

For the commands below, install
[`vitacompanion`](https://github.com/devnoname120/vitacompanion) on the Vita and
leave its FTP server running.

### First installation

```sh
make upload-vpk VITA_IP=192.168.0.103
```

This uploads the package to `ux0:/data/green-vita.vpk`; it does not install it.
Open VitaShell and install that file once. To choose another upload directory:

```sh
make upload-vpk VITA_IP=192.168.0.103 VITA_UPLOAD_DIR=ux0:/downloads/
```

### Fast update and run

After the VPK is installed:

```sh
make update-run-vita VITA_IP=192.168.0.103
```

This rebuilds `eboot.bin`, replaces `ux0:/app/GREENVITA/eboot.bin`, and starts
the application. `make run-vita VITA_IP=...` is an alias for the same command.

> [!CAUTION]
> The update command replaces only `eboot.bin`. Reinstall the complete VPK
> whenever package metadata or files under `static/` change.

<details>
<summary>FTP error 550: File not found</summary>

The destination directory must already exist. Install the VPK before using
`update-run-vita`, and make sure the upload directory passed through
`VITA_UPLOAD_DIR` exists on the Vita memory card.

</details>
## Credits

- [Greenlight](https://github.com/unknownskl/greenlight), an open-source xCloud
  and Xbox home-streaming client that served as a protocol and UX reference
- [xbox-xcloud-player](https://github.com/unknownskl/xbox-xcloud-player), the
  WebRTC streaming library used by Greenlight and a key reference for xCloud
  and xHome session handling
- [Vita Moonlight](https://github.com/xyzz/vita-moonlight), a major reference
  for low-latency streaming and hardware video decoding on the PS Vita
- PS Vita icon used in the GreenVita logo: "PS Vita" by Mark Davis from
  [The Noun Project](https://thenounproject.com/icon/ps-vita-203775/)
- [VitaSDK](https://vitasdk.org/) and the
  [vita-rust](https://github.com/vita-rust) ecosystem

GreenVita is an independent homebrew project and is not affiliated with or
endorsed by Microsoft, Xbox, Sony, or PlayStation.

## License

GreenVita is licensed under the [Mozilla Public License 2.0](LICENSE).
