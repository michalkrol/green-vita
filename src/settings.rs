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
    /// Known limitation: Xbox One VCE may not encode High profile; selecting it
    /// may cause a WebRTC ICE-ufrag error. If that happens, switch back to Main.
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub locale: Locale,
    /// Requests and presents up to 60 FPS. Off by default to protect Vita performance.
    pub unlock_video_fps: bool,
    /// Shows internal stream/session state on the `Streaming` screen. Off by default.
    pub show_stream_debug_info: bool,
    /// Shows a minimal FPS + bandwidth overlay at the top-right corner.
    pub show_fps_overlay: bool,
    /// Home-console LAN IPv4 override (e.g. "192.168.0.123"). When set, home-stream ICE
    /// candidates target this address directly instead of the Teredo-decoded WAN endpoint,
    /// bypassing router NAT hairpin (which can be slow/lossy and cause growing video lag).
    pub home_console_ip: Option<String>,
    /// Video bitrate cap in kbps sent to the console via SDP (b=AS / b=TIAS).
    /// Range 2 000–50 000 kbps, default 15 000 (15 Mbps). Higher values can improve
    /// visual quality at the cost of network bandwidth and potentially more drift.
    pub video_bitrate_kbps: u32,
    /// H264 profile offered to the console in the SDP. Baseline is universally supported
    /// by the Vita's HW decoder. Main and High may produce artifacts or fail entirely
    /// depending on tile/bitstream compatibility.
    pub video_h264_profile: H264Profile,
    /// When true, a keyframe (IDR) is requested from the console every 200 ms via
    /// RTCP PLI.  This clears decoder-reference corruption (blocky artifacts) at the
    /// cost of slightly larger periodic frames and a small bandwidth overhead.
    pub periodic_keyframe: bool,
    /// Decode loop sleep in ms (default 13). Lower values reduce latency but increase CPU.
    pub video_decode_sleep_ms: u32,
    /// Decode queue depth (default 6). Lower values reduce buffering but risk dropped frames.
    pub video_decode_queue_depth: u32,
    /// When enabled, auto-triggers a REMB shock (drops bitrate to 100 kbps briefly)
    /// when the frame drop rate exceeds the threshold. The goal is to force the
    /// console's encoder to flush its internal pre-stamp queue.
    pub remb_auto_shock_enabled: bool,
    /// New drops since last shock required to trigger another shock (default 10).
    pub remb_shock_drop_gap: u32,
    /// Minimum seconds between shocks (default 15).
    pub remb_shock_cooldown_secs: u32,
    /// Duration of the REMB low-bitrate pulse in ms (default 100).
    pub remb_shock_duration_ms: u32,
    /// Globally swaps L1↔L2 and R1↔R2 shoulder/trigger mappings for all streams
    /// (local xHome and cloud). Per-game profiles override this when set.
    pub swap_shoulders_and_triggers: bool,
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
            unlock_video_fps: false,
            show_stream_debug_info: false,
            show_fps_overlay: false,
            home_console_ip: None,
            video_bitrate_kbps: 15_000,
            video_h264_profile: H264Profile::default(),
            periodic_keyframe: true,
            video_decode_sleep_ms: 0,
            video_decode_queue_depth: 1,
            remb_auto_shock_enabled: false,
            remb_shock_drop_gap: 10,
            remb_shock_cooldown_secs: 15,
            remb_shock_duration_ms: 100,
            swap_shoulders_and_triggers: false,
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
