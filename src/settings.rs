//! User-editable settings, persisted to `ux0:data/xcloud-rust/settings.json` on each change.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const SETTINGS_DIR: &str = "ux0:data/xcloud-rust";
const SETTINGS_PATH: &str = "ux0:data/xcloud-rust/settings.json";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    EnUs,
    EnGb,
    PtBr,
    PtPt,
    EsEs,
    EsMx,
    FrFr,
    DeDe,
    ItIt,
    JaJp,
    KoKr,
    ZhCn,
    ZhTw,
    RuRu,
    PlPl,
    NlNl,
    SvSe,
    TrTr,
    ArSa,
}

impl Locale {
    pub const ALL: [Locale; 19] = [
        Self::EnUs,
        Self::EnGb,
        Self::PtBr,
        Self::PtPt,
        Self::EsEs,
        Self::EsMx,
        Self::FrFr,
        Self::DeDe,
        Self::ItIt,
        Self::JaJp,
        Self::KoKr,
        Self::ZhCn,
        Self::ZhTw,
        Self::RuRu,
        Self::PlPl,
        Self::NlNl,
        Self::SvSe,
        Self::TrTr,
        Self::ArSa,
    ];

    /// `(locale code, store market, native-language label)`.
    fn info(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::EnUs => ("en-US", "US", "English (US)"),
            Self::EnGb => ("en-GB", "GB", "English (UK)"),
            Self::PtBr => ("pt-BR", "BR", "Português (Brasil)"),
            Self::PtPt => ("pt-PT", "PT", "Português (Portugal)"),
            Self::EsEs => ("es-ES", "ES", "Español (España)"),
            Self::EsMx => ("es-MX", "MX", "Español (México)"),
            Self::FrFr => ("fr-FR", "FR", "Français"),
            Self::DeDe => ("de-DE", "DE", "Deutsch"),
            Self::ItIt => ("it-IT", "IT", "Italiano"),
            Self::JaJp => ("ja-JP", "JP", "日本語"),
            Self::KoKr => ("ko-KR", "KR", "한국어"),
            Self::ZhCn => ("zh-CN", "CN", "中文（简体）"),
            Self::ZhTw => ("zh-TW", "TW", "中文（繁體）"),
            Self::RuRu => ("ru-RU", "RU", "Русский"),
            Self::PlPl => ("pl-PL", "PL", "Polski"),
            Self::NlNl => ("nl-NL", "NL", "Nederlands"),
            Self::SvSe => ("sv-SE", "SE", "Svenska"),
            Self::TrTr => ("tr-TR", "TR", "Türkçe"),
            Self::ArSa => ("ar-SA", "SA", "العربية"),
        }
    }

    /// The locale code sent to xCloud, e.g. `"pt-BR"`.
    pub fn as_str(self) -> &'static str {
        self.info().0
    }

    /// The Microsoft Store catalog's `market` query param, e.g. `"BR"`.
    pub fn market(self) -> &'static str {
        self.info().1
    }

    /// Display label in the language's own native name, e.g. `"Русский"`.
    pub fn label(self) -> &'static str {
        self.info().2
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum H264Profile {
    #[default]
    Baseline,
    Main,
}

/// Mini debug mode: bitrate/bandwidth + displayed FPS only, white text on semi-transparent dark rectangle, TOP-RIGHT corner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamDebugMode {
    #[default]
    Off,
    /// Mini overlay (TOP-RIGHT): measured bandwidth Mbps + displayed FPS.
    Minimal,
    /// The existing full debug overlay, unchanged.
    Full,
}

/// What happens when an access unit resolves Damaged (unrepairable in its
/// grace/RTX window). Step 6 (2026-11-02): per-game artefact-vs-freeze
/// choice. Damaged AUs are never decoded — the hardware decoder conceals on
/// the FOLLOWING frames (macroblocking) until the PLI→IDR reset; the
/// Quality/Balanced policies shed-to-IDR instead, trading a micro-freeze
/// per damage event for artefact-free output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DamagePolicy {
    /// Drop + decode-through: the decoder conceals on the next frames
    /// (brief artefacts, no stall). Default.
    #[default]
    LowLatency,
    /// Drop; immediate shed-to-IDR only on severe damage (leading slice
    /// lost or multiple holes) — worst concealment avoided.
    Balanced,
    /// Drop + immediate shed-to-IDR on EVERY damaged frame — artefact-free,
    /// accepts a micro-freeze per event.
    Quality,
}

/// Repair-window policy (perf plan 2026-11-25): how long the assembler may
/// hold the reference chain on a damaged AU while RTX retransmissions fill
/// its holes. A park avoids a PLI/IDR and persistent artefacts at the cost of
/// a bounded chain freeze (the screen holds the last good picture); the
/// follow-up frames are decoded in order after the fill, so nothing stale is
/// ever published. Presets map to the RepairPolicy table in assembler.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RepairWindow {
    /// No park: damage resolves immediately (no RTX repair window).
    ///
    /// Default since 2026-11-26: the park holds the whole queue for the
    /// window, so Balanced (40 ms) and Patient (60 ms) produced visible
    /// freezes on every damaged AU (user: "Repair window Off - no freezes;
    /// Balanced - many; Patient - a lot, unplayable"). Repair quality is
    /// better with a window, but playability wins by default; the setting
    /// remains for quality-first sessions.
    #[default]
    Off,
    /// 17 ms park — tightest chain hold; loses fills that land later.
    Short,
    /// 40 ms park — the pre-2026-11-25 fixed budget.
    Balanced,
    /// 60 ms park — longest chain hold, highest fill rate.
    Patient,
}

/// Hardware decoder output color profile. RGB565 (default) halves the
/// decoder-to-texture CDRAM traffic vs RGBA8888; RGBA8888 (TODO item 3) is the
/// optional quality mode that eliminates RGB565 dark-scene banding ("reads as
/// blockiness") at the cost of doubled per-frame decode output bandwidth
/// (LATENCY-FINDINGS-2026-10-23). Applies at the next stream start — the
/// decoder output format is fixed at decoder construction. NOTE: this changes
/// nothing on the wire — the H.264 bitstream (and thus Wi-Fi throughput) is
/// identical; only local memory traffic differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorProfile {
    /// Hardware RGB565 direct decode (default; lowest memory bandwidth).
    #[default]
    Rgb565,
    /// Hardware RGBA8888 direct decode — true 8-bit channels, no banding.
    Rgba8888,
}

/// Decode-bank depth profile (item 15): Fast = shallow bank (target 0,
    /// bursts shed or conceal — minimal floor); Smooth = deep bank (target 6,
    /// hard cap 12 ≈ +100–200 ms floor) — bursts and damage-shed stalls
    /// absorbed invisibly (the 0.3.1 quality recreation, accumulation-safe: the
    /// bank is hard-capped in admission and TWCC stamps pre-bank).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LatencyProfile {
    /// Bank target 0, boost ≤2 (+33 ms worst case). Default.
    #[default]
    Fast,
    /// Bank target 6, admission limit 12 (≈ +100–200 ms floor).
    Smooth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub locale: Locale,
    /// Video bitrate cap in kbps sent to the console via SDP (b=AS / b=TIAS).
    /// NOTE: the old `unlock_video_fps` 60/30 switch was removed (2026-11-03)
    /// as misleading — the console encodes ~60 fps regardless of the
    /// `a=framerate` SDP attribute (54-session src_fps survey: 58 median
    /// across every era, cap, and setting). The offer always requests 60.
    pub video_bitrate_kbps: u32,
    /// Shows internal stream/session state on the `Streaming` screen. Off by default.
    pub stream_debug_mode: StreamDebugMode,
    /// Home-console LAN IPv4 override (e.g. "192.168.0.123"). When set, home-stream ICE
    /// candidates target this address directly instead of the Teredo-decoded WAN endpoint,
    /// bypassing router NAT hairpin (which can be slow/lossy and cause growing video lag).
    pub home_console_ip: Option<String>,
    /// H264 profile offered to the console in the SDP. Baseline is universally supported
    /// by the Vita's HW decoder. Main and High may produce artifacts or fail entirely
    /// depending on tile/bitstream compatibility.
    pub video_h264_profile: H264Profile,
    /// What happens when an access unit resolves Damaged. LowLatency (default)
    /// decodes through (decoder conceals — brief artefacts); Balanced sheds to
    /// IDR on severe damage; Quality sheds on every damaged frame
    /// (artefact-free, micro-freeze per event). See DamagePolicy.
    pub damage_policy: DamagePolicy,
    /// Repair-window policy: how long a damaged AU may be held for RTX repair
    /// before resolving Damaged (PLI + decode-through concealment). See RepairWindow.
    pub repair_window: RepairWindow,
    /// Manual 60ms TWCC feedback cadence (per-packet transport-wide congestion
    /// reports to the console); helps lossy links, can regress good ones.
    /// Applies live — the per-packet gate reads the atomic every packet.
    /// Hidden (no settings row): manual 60 ms TWCC feedback. Default off — the
    /// plan found it regresses good links (3.2x less damage on bad links, but
    /// regressions on good ones). Toggle via settings.json or at compile time.
    pub manual_twcc_60ms: bool,
    /// Decode-bank depth profile (item 15): Fast = shallow bank (target 0,
    /// bursts shed or conceal — minimal floor); Smooth = deep bank (target 6,
    /// hard cap 12 ≈ +100–200 ms floor) — bursts and damage-shed stalls
    /// absorbed invisibly (the 0.3.1 quality recreation, accumulation-safe: the bank
    /// is hard-capped in admission and TWCC stamps pre-bank).
    pub latency_profile: LatencyProfile,
    /// Hardware decoder output color profile (ColorProfile). Rgb565 default;
    /// Rgba8888 kills 565 dark-scene banding at +memory-bandwidth cost
    /// (TODO item 3). Takes effect on the next stream start.
    pub color_profile: ColorProfile,
    /// Swap L1/L2 with R1/R2 (shoulders and triggers) for every title, not just
    /// titles with the per-game toggle enabled.
    pub swap_shoulders_and_triggers: bool,
    /// When true, latency test mode is active - shows a blinking square and stopwatch
    /// on button press to measure glass-to-glass latency with a phone camera.
    pub latency_test_mode: bool,
    /// Capture the PS button while streaming (tap = Xbox guide, hold =
    /// Vita menu passthrough); experimental.
    pub ps_button_capture: bool,
    pub game_profiles: HashMap<String, GameProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GameProfile {
    pub swap_shoulders_and_triggers: bool,
    /// When enabled, front touch mirrors the rear L2/R2/L3/R3 zones instead of acting as a
    /// clickable xCloud pointer. Defaults to pointer mode for existing profiles.
    pub front_touch_auxiliary_buttons: bool,
    /// Controls only input from the physical rear touch panel. Front touch remains independent.
    pub rear_touch_enabled: bool,
}

impl Default for GameProfile {
    fn default() -> Self {
        Self {
            swap_shoulders_and_triggers: false,
            front_touch_auxiliary_buttons: false,
            rear_touch_enabled: true,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            locale: Locale::default(),
            stream_debug_mode: StreamDebugMode::default(),
            home_console_ip: None,
            video_bitrate_kbps: 15_000,
            video_h264_profile: H264Profile::default(),
            damage_policy: DamagePolicy::default(),
            repair_window: RepairWindow::default(),
            latency_profile: LatencyProfile::default(),
            manual_twcc_60ms: false,
            color_profile: ColorProfile::default(),
            swap_shoulders_and_triggers: false,
            latency_test_mode: false,
            ps_button_capture: false,
            game_profiles: HashMap::new(),
        }
    }
}

impl Settings {
    pub fn game_profile(&self, title_id: &str) -> Option<&GameProfile> {
        self.game_profiles.get(title_id)
    }

    pub fn set_swap_shoulders_and_triggers(&mut self, title_id: String, enabled: bool) {
        self.game_profiles
            .entry(title_id)
            .or_default()
            .swap_shoulders_and_triggers = enabled;
    }

    pub fn set_front_touch_auxiliary_buttons(&mut self, title_id: String, enabled: bool) {
        self.game_profiles
            .entry(title_id)
            .or_default()
            .front_touch_auxiliary_buttons = enabled;
    }

    pub fn set_rear_touch_enabled(&mut self, title_id: String, enabled: bool) {
        self.game_profiles
            .entry(title_id)
            .or_default()
            .rear_touch_enabled = enabled;
    }

    /// Loads from disk, falling back to defaults on any error.
    pub fn load() -> Self {
        let data = match std::fs::read_to_string(SETTINGS_PATH) {
            Ok(data) => data,
            Err(error) => {
                eprintln!("Settings: no existing {SETTINGS_PATH} ({error}), using defaults");
                return Self::default();
            }
        };
        match serde_json::from_str(&data) {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!("Settings: failed to parse {SETTINGS_PATH} ({error}), using defaults");
                Self::default()
            }
        }
    }

    /// Saves to disk; errors are logged rather than surfaced.
    pub fn save(&self) {
        let result = std::fs::create_dir_all(SETTINGS_DIR)
            .context("failed to create settings directory")
            .and_then(|_| {
                serde_json::to_string_pretty(self).context("failed to serialize settings")
            })
            .and_then(|data| crate::fs_utils::write_file_truncating(SETTINGS_PATH, data));

        if let Err(error) = result {
            eprintln!("Settings: failed to save: {error:#}");
        }
    }
}
