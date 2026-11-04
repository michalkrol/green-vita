<p align="center">
  <img src="assets/greenvita.svg" alt="GreenVita" width="220">
</p>

<h1 align="center">GreenVita</h1>

<p align="center">
  Xbox Cloud Gaming and Xbox Remote Play (home console streaming) on PlayStation Vita.
  <br>
  A native Rust client with an egui/SDL2 interface and hardware H.264 decoding.
</p>

<p align="center">
  <img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-f74c00?style=for-the-badge&logo=rust&logoColor=white">
  <img alt="PS Vita homebrew" src="https://img.shields.io/badge/PS%20Vita-homebrew-44aa00?style=for-the-badge">
  <img alt="Xbox Cloud Gaming" src="https://img.shields.io/badge/Xbox-Cloud%20Gaming-107c10?style=for-the-badge&logo=xbox&logoColor=white">
</p>

> [!NOTE]
> **About this fork:** this GreenVita fork focuses on **local console
> streaming (xHome)** — streaming from your own Xbox over the local network.
> The xHome path is hardware-validated end-to-end (sign-in, session
> brokering, WebRTC, hardware H.264 decode, input and audio) and is under
> active quality development: packet-loss recovery, Wi-Fi-burst absorption
> and latency tooling. Xbox Cloud Gaming (xCloud) still builds and launches,
> but it is not the focus of this fork and is not actively tested.

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

## Recommended: Wi-Fi Turbo plugin

For local streaming, install [Wi-Fi Turbo for PS Vita](https://github.com/juanjsebgarcia/vita-turbo-wifi) — a taiHEN plugin that
fixes the Vita's 2011-era Wi-Fi defaults. 

This directly addresses GreenVita's main remaining video issue: our captures
show the console's over-the-air delivery can be lossless — damaged frames and
micro-freezes come from the Vita's own 2.4 GHz radio behaviour (power-save
stalls of 100–200 ms, 20 MHz airtime saturation). Wi-Fi Turbo removes exactly
those.

Recommended while streaming: **Max performance** power profile, **40 MHz on**
if your access point offers a 40 MHz 2.4 GHz channel (**turn it off in congested
areas** - 20MHz will perform better in WiFi-dense areas), Bluetooth off. 
Configure your Access Point to least crowded channel (1, 6 or 11). 
It is an independent project (firmware 3.60–3.74) with its own installer — see its README. 
Unrelated to GreenVita; install it separately.

## Streaming settings

- **Latency profile — Fast / Smooth** (switchable mid-stream). *Fast*
  (default) keeps the minimal glass-to-glass floor; *Smooth* uses a deeper
  frame bank that silently absorbs Wi-Fi bursts and damage-repair stalls,
  at the cost of ~100–200 ms extra latency.
- **Damage policy — Low latency / Balanced / Quality.** What to do when a
  frame arrives damaged: decode through it (default), wait for a clean
  keyframe only on severe damage, or always request a clean keyframe
  (artifact-free picture with a brief freeze per event).
- **Bitrate cap advice.** Before sign-in the app probes the network and may
  suggest a bitrate cap via an advisory banner. Applying it is one tap and
  never automatic.
- **Debug overlay — Off / FPS+Bitrate / Full.** Small on-screen diagnostics.

## Diagnostics

During a stream a HUD line shows fps, decode/drop counters and handshake
state (see [docs/HUD-REFERENCE.md](docs/HUD-REFERENCE.md)). Each session
appends one row/second to `ux0:data/xcloud-rust/metrics.csv`
(100 columns — [docs/METRICS-CSV-REFERENCE.md](docs/METRICS-CSV-REFERENCE.md)).
Streaming design notes: [docs/DESIGN-NOTES.md](docs/DESIGN-NOTES.md).


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

- [Day-OS/green-vita](https://github.com/day-os/green-vita), the founder of green-vita project this fork is based on.
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
