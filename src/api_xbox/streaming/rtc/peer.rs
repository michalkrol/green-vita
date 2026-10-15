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
    MIME_TYPE_H264, MIME_TYPE_OPUS, MIME_TYPE_RTX, MediaEngine,
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

/// Flip to false for the "no audio negotiation" test build.
const AUDIO_NEGOTIATION_ENABLED: bool = true;

fn register_vita_codecs(media_engine: &mut MediaEngine, video_fps: u32, video_h264_profile: H264Profile) -> Result<()> {
    // Offer every H.264 profile the Vita's decoder can handle, user-selected
    // profile first. The console requires at least one acceptable profile in
    // the m=video line — offering only one (especially High) gets m=video
    // rejected with a BUNDLE/ice-ufrag mismatch downstream.
    let baseline = h264_profile_level_id(video_fps, H264Profile::Baseline);
    let main = h264_profile_level_id(video_fps, H264Profile::Main);
    let high = h264_profile_level_id(video_fps, H264Profile::High);
    let ordered: [&'static str; 3] = match video_h264_profile {
        H264Profile::Baseline => [baseline, main, high],
        H264Profile::Main => [main, baseline, high],
        H264Profile::High => [high, baseline, main],
    };

    let mut next_pt: u8 = 102;
    let mut h264_pts: Vec<u8> = Vec::new();
    for level in ordered {
        media_engine.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: RTCRtpCodec {
                    mime_type: MIME_TYPE_H264.to_owned(),
                    clock_rate: 90_000,
                    channels: 0,
                    sdp_fmtp_line: format!(
                        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={level}"
                    ),
                    rtcp_feedback: vec![
                        RTCPFeedback { typ: "goog-remb".to_owned(), parameter: "".to_owned() },
                        RTCPFeedback { typ: "ccm".to_owned(), parameter: "fir".to_owned() },
                        RTCPFeedback { typ: "nack".to_owned(), parameter: "".to_owned() },
                        RTCPFeedback { typ: "nack".to_owned(), parameter: "pli".to_owned() },
                    ],
                },
                payload_type: next_pt,
            },
            RtpCodecKind::Video,
        )?;
        h264_pts.push(next_pt);
        next_pt += 1;
    }

    // RTX pair for each H.264 PT (Chrome-style apt= mapping). Chrome negotiates
    // this and the console opens its RFC 4588 retransmission path for it; our
    // client previously advertised neither `nack` nor RTX, so every loss
    // escalated to keyframe/flush storms that backlogged the Xbox One encoder.
    for primary_pt in &h264_pts {
        media_engine.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: RTCRtpCodec {
                    mime_type: MIME_TYPE_RTX.to_owned(),
                    clock_rate: 90_000,
                    channels: 0,
                    sdp_fmtp_line: format!("apt={primary_pt}"),
                    rtcp_feedback: vec![],
                },
                payload_type: next_pt,
            },
            RtpCodecKind::Video,
        )?;
        next_pt += 1;
    }

    // Audio codec (omitted entirely for the no-audio negotiation test build).
    if AUDIO_NEGOTIATION_ENABLED {
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
    }

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
        assert_eq!(h264_profile_level_id(30, H264Profile::High), "64001f");
        assert_eq!(h264_profile_level_id(60, H264Profile::High), "640020");
    }
}
