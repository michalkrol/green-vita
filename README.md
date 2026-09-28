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

Overclocking the Vita CPU/GPU does not improve streaming performance — the
bottleneck is WiFi airtime, not decode speed.

Recommendations:
- Use 20 MHz channel width on your router (not 40 MHz — causes retransmits)
- Pick an unused channel (1, 6, or 11)
- Turn Bluetooth OFF in Vita system settings (shares the same 2.4 GHz antenna)

### Stream profiles

The Stream profile setting batch-tunes all advanced parameters for known
scenarios. Changing profiles requires a video reconnection.

| Profile | Bandwidth | H264 | Sleep | Queue | HUD | When to use |
|---------|-----------|------|-------|-------|-----|-------------|
| **Game** | 2000 kbps | Baseline | 0 ms | 1 | Hidden | Fast-paced games (racing, FPS) |
| **Media** | 7500 kbps | High | 2 ms | 4 | Visible | Slow-paced/story games |
| **Custom** | User-set | User-set | User-set | User-set | Configurable | Fine-tune every knob |

### Advanced stream config — Custom profile

#### Bandwidth cap
0 = unlimited, 500-8000 kbps. Lower = faster response, fewer artifacts.

#### H264 profile
Baseline (fast decode): no B-frames, any queue depth
Main (balanced): B-frames, requires queue depth 3+
High (image quality): B-frames + CABAC, requires queue depth 3+

#### Decode sleep
0 ms = fastest decode, may cause artifacts
1 ms = safe default
2 ms = safer on noisy WiFi

#### Decode queue depth
Main/High profiles require 3+. 1 = ~60ms latency, 8-10 = smoothest.

#### Hard bandwidth cap
REMB feedback at 500ms to enforce the bandwidth ceiling.

#### Disable UI overlay
Bypasses egui entirely, saving ~8.3ms display latency (half a frame at 60 FPS).

---

## Install

You need a homebrew-enabled PS Vita with VitaShell and an Xbox account that has
access to Xbox Cloud Gaming.

1. Download `green-vita.vpk` from the
   [latest release](https://github.com/Day-OS/green-vita/releases/latest).
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
