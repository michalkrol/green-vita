use crate::Stream;
use crate::api::streaming::rtc::session::RtcSessionConfig;
use crate::api::streaming::rtc::worker::{RtcWorker, RtcWorkerProvider};
use crate::api_xbox::streaming::rtc::protocol::XboxRtcProtocol;
use crate::api_xbox::streaming::rtc::{AUDIO_PAYLOAD_TYPE, ROUTE_PROBE, STUN_SERVER, peer, sdp};
use crate::streaming::audio::AUDIO_SAMPLE_RATE;
use crate::streaming::video::{
    DEFAULT_VIDEO_FPS, DecoderConfig, HW_OUTPUT_HEIGHT, HW_OUTPUT_WIDTH, STREAM_HEIGHT,
    STREAM_WIDTH, UNLOCKED_VIDEO_FPS,
};
use anyhow::Result;
use rtc::peer_connection::sdp::RTCSessionDescription;

use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use crate::settings::H264Profile;

struct XboxRtcWorkerProvider {
    stream: Stream,
    video_fps: u32,
    video_bitrate_kbps: u32,
    video_h264_profile: H264Profile,
    periodic_keyframe: bool,
    decode_sleep_ms: u32,
    decode_queue_depth: usize,
    remb_auto_shock_enabled: bool,
    remb_shock_drop_gap: u32,
    remb_shock_cooldown_secs: u32,
    remb_shock_duration_ms: u32,
}

impl RtcWorkerProvider for XboxRtcWorkerProvider {
    type Protocol = XboxRtcProtocol;

    fn create_peer(&self) -> Result<(RTCPeerConnection, Self::Protocol)> {
        peer::create(self.video_fps, self.video_h264_profile)
    }

    fn session_config(&self) -> RtcSessionConfig {
        RtcSessionConfig {
            stun_server: STUN_SERVER,
            route_probe: ROUTE_PROBE,
            audio_sample_rate: AUDIO_SAMPLE_RATE as u32,
            audio_payload_type: AUDIO_PAYLOAD_TYPE,
            video_fps: self.video_fps,
            decoder: DecoderConfig {
                decode_width: STREAM_WIDTH,
                decode_height: STREAM_HEIGHT,
                output_width: HW_OUTPUT_WIDTH,
                output_height: HW_OUTPUT_HEIGHT,
                decode_sleep_ms: self.decode_sleep_ms,
                decode_queue_depth: self.decode_queue_depth,
            },
            periodic_keyframe_enabled: self.periodic_keyframe,
            remb_auto_shock_enabled: self.remb_auto_shock_enabled,
            remb_shock_drop_gap: self.remb_shock_drop_gap,
            remb_shock_cooldown_secs: self.remb_shock_cooldown_secs,
            remb_shock_duration_ms: self.remb_shock_duration_ms,
        }
    }

    async fn exchange_sdp(&self, offer: &RTCSessionDescription) -> Result<String> {
        let sdp = sdp::cap_video_bitrate(&offer.sdp, self.video_bitrate_kbps);
        let sdp = sdp::enable_transport_cc(&sdp);
        let sdp = sdp::request_video_fps(&sdp, self.video_fps);
        let answer = self.stream.send_sdp_offer(&sdp).await?;
        // Try both URIs; the console may echo draft-holmer-rmcat or transport-wide-cc-02.
        let twcc_id = sdp::extract_extmap_id(&answer, super::peer::TWCC_URI)
            .or_else(|| sdp::extract_extmap_id(&answer, "http://www.webrtc.org/experiments/rtp-hdrext/transport-wide-cc-02"))
            .unwrap_or(0);
        eprintln!("negotiated twcc ext id: {twcc_id}");
        crate::api::streaming::rtc::rtp::set_negotiated_twcc_ext_id(twcc_id);
        Ok(answer)
    }
}

pub(crate) fn spawn(
    stream: Stream,
    unlock_video_fps: bool,
    video_bitrate_kbps: u32,
    video_h264_profile: H264Profile,
    periodic_keyframe: bool,
    decode_sleep_ms: u32,
    decode_queue_depth: u32,
    remb_auto_shock_enabled: bool,
    remb_shock_drop_gap: u32,
    remb_shock_cooldown_secs: u32,
    remb_shock_duration_ms: u32,
) -> Result<RtcWorker> {
    let video_fps = if unlock_video_fps {
        UNLOCKED_VIDEO_FPS
    } else {
        DEFAULT_VIDEO_FPS
    };
    RtcWorker::spawn(XboxRtcWorkerProvider {
        stream,
        video_fps,
        video_bitrate_kbps,
        video_h264_profile,
        periodic_keyframe,
        decode_sleep_ms,
        decode_queue_depth: decode_queue_depth as usize,
        remb_auto_shock_enabled,
        remb_shock_drop_gap,
        remb_shock_cooldown_secs,
        remb_shock_duration_ms,
    })
}
