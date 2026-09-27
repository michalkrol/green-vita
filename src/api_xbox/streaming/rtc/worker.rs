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
use std::sync::atomic::Ordering;

use crate::api::streaming::rtc::media::REMB_BPS;

use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use crate::settings::H264Profile;

struct XboxRtcWorkerProvider {
    stream: Stream,
    video_fps: u32,
    video_bitrate_kbps: u32,
    video_h264_profile: H264Profile,
    decode_sleep_ms: u32,
    decode_queue_depth: usize,
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
    decode_sleep_ms: u32,
    decode_queue_depth: u32,
    hard_bandwidth_cap: bool,
) -> Result<RtcWorker> {
    let video_fps = if unlock_video_fps {
        UNLOCKED_VIDEO_FPS
    } else {
        DEFAULT_VIDEO_FPS
    };
    // Wire REMB target to the user's bandwidth cap setting.
    // 0 = no cap → 15 Mbps default (console manages its own rate).
    let remb_bps = if video_bitrate_kbps > 0 && hard_bandwidth_cap {
        video_bitrate_kbps * 1000
    } else {
        15_000_000
    };
    REMB_BPS.store(remb_bps, Ordering::Release);
    RtcWorker::spawn(XboxRtcWorkerProvider {
        stream,
        video_fps,
        video_bitrate_kbps,
        video_h264_profile,
        decode_sleep_ms,
        decode_queue_depth: decode_queue_depth as usize,
    })
}
