use crate::api::streaming::rtc::peer;
use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use crate::api_xbox::streaming::control::channel::{
    CHAT_CHANNEL, CONTROL_CHANNEL, INPUT_CHANNEL, MESSAGE_CHANNEL,
};
use crate::api_xbox::streaming::rtc::protocol::{ChannelIds, XboxRtcProtocol};
use crate::api_xbox::streaming::rtc::{AUDIO_PAYLOAD_TYPE, STUN_SERVER};
use crate::settings::H264Profile;
use anyhow::{Context, Result};
use rtc::peer_connection::configuration::media_engine::{
    MIME_TYPE_H264, MIME_TYPE_OPUS, MediaEngine,
};
use rtc::peer_connection::transport::RTCIceServer;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RtpCodecKind,
};

/// transport-wide-cc URI kept for answer SDP parsing in worker.rs; the extension
/// is no longer offered since the console never annotates toward us at useful density.
pub(super) const TWCC_URI: &str =
    "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";

pub(super) fn create(video_fps: u32, video_h264_profile: H264Profile) -> Result<(RTCPeerConnection, XboxRtcProtocol)> {
    let mut media_engine = MediaEngine::default();
    register_vita_codecs(&mut media_engine, video_fps, video_h264_profile).context("failed to register Vita codecs")?;

    let (peer_connection, ids) = peer::create(
        media_engine,
        vec![RTCIceServer {
            urls: vec![format!("stun:{STUN_SERVER}")],
            username: String::new(),
            credential: String::new(),
        }],
        &[
            CHAT_CHANNEL,
            CONTROL_CHANNEL,
            INPUT_CHANNEL,
            MESSAGE_CHANNEL,
        ],
    )?;

    // `ids[0]` (chat) is created for protocol compatibility with the remote peer, but its id
    // is never needed locally.
    let channel_ids = ChannelIds {
        control: ids[1],
        input: ids[2],
        message: ids[3],
    };

    Ok((
        peer_connection,
        XboxRtcProtocol::new(channel_ids, video_fps),
    ))
}

fn register_vita_codecs(media_engine: &mut MediaEngine, video_fps: u32, video_h264_profile: H264Profile) -> Result<()> {
    // Bare-minimum offer: H264 video + Opus audio, only the feedback types the
    // console actually echoes (goog-remb, ccm fir, nack pli). No header extensions,
    // no RTX, no FEC, no extra profiles — these all proved inert for drift and
    // some (#21 annotation gating, #35 browser-shape SDP) actively regressive.

    media_engine.register_codec(
        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1".to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: AUDIO_PAYLOAD_TYPE,
        },
        RtpCodecKind::Audio,
    )?;

    media_engine.register_codec(
        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: format!(
                    "level-asymmetry-allowed=0;packetization-mode=1;profile-level-id={};max-dpb-size=1;max-bframes=0",
                    h264_profile_level_id(video_fps, video_h264_profile)
                ),
                rtcp_feedback: vec![
                    RTCPFeedback { typ: "goog-remb".to_owned(), parameter: "".to_owned() },
                    RTCPFeedback { typ: "ccm".to_owned(), parameter: "fir".to_owned() },
                    RTCPFeedback { typ: "nack".to_owned(), parameter: "pli".to_owned() },
                ],
            },
            payload_type: 102,
        },
        RtpCodecKind::Video,
    )?;

    Ok(())
}

fn h264_profile_level_id(video_fps: u32, video_h264_profile: H264Profile) -> &'static str {
    match (video_h264_profile, video_fps > 30) {
        (H264Profile::Baseline, false) => "42e01f",
        (H264Profile::Baseline, true) => "42e020",
        (H264Profile::Main, false) => "4de01f",
        (H264Profile::Main, true) => "4de020",
        (H264Profile::High, false) => "64001f",
        (H264Profile::High, true) => "640020",
    }
}

#[cfg(test)]
mod tests {
    use super::h264_profile_level_id;
    use crate::settings::H264Profile;

    #[test]
    fn selects_profile_level_id() {
        assert_eq!(h264_profile_level_id(30, H264Profile::Baseline), "42e01f");
        assert_eq!(h264_profile_level_id(60, H264Profile::Baseline), "42e020");
        assert_eq!(h264_profile_level_id(30, H264Profile::Main), "4de01f");
        assert_eq!(h264_profile_level_id(60, H264Profile::Main), "4de020");
    }
}
