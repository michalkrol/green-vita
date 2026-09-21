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
            },
            periodic_keyframe_enabled: self.periodic_keyframe,
        }
    }

    async fn exchange_sdp(&self, offer: &RTCSessionDescription) -> Result<String> {
        let sdp = sdp::cap_video_bitrate(&offer.sdp, self.video_bitrate_kbps);
        let sdp = sdp::request_video_fps(&sdp, 30);
        let answer = self.stream.send_sdp_offer(&sdp).await?;
        let twcc_id = sdp::extract_extmap_id(&answer, super::peer::TWCC_URI).unwrap_or(0);
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
    })
}
