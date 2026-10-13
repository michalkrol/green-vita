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
    // Offer the selected profile at PT 102, PLUS a fallback Constrained Baseline
    // at PT 103. The xHome console requires at least one profile it supports
    // in the m=video line — offering only one (especially High) gets rejected
    // with m=video 0 and a BUNDLE/ice-ufrag mismatch downstream.

    // Primary: user-selected profile at PT 102
    media_engine.register_codec(
        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: format!(
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}",
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

    // Fallback: Constrained Baseline at PT 103 (always offered, ensures console
    // has at least one acceptable codec to pick)
    if video_h264_profile != H264Profile::Baseline {
        media_engine.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: RTCRtpCodec {
                    mime_type: MIME_TYPE_H264.to_owned(),
                    clock_rate: 90_000,
                    channels: 0,
                    sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
                    rtcp_feedback: vec![
                        RTCPFeedback { typ: "goog-remb".to_owned(), parameter: "".to_owned() },
                        RTCPFeedback { typ: "ccm".to_owned(), parameter: "fir".to_owned() },
                        RTCPFeedback { typ: "nack".to_owned(), parameter: "pli".to_owned() },
                    ],
                },
                payload_type: 103,
            },
            RtpCodecKind::Video,
        )?;
    }

    // Audio codec
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

    Ok(())
}

fn h264_profile_level_id(video_fps: u32, video_h264_profile: H264Profile) -> &'static str {
    // profile-level-id encoding per RFC 6184 / H.264:
    // byte 0 = profile_idc (66=Baseline, 77=Main, 100=High)
    // bytes 1-2 = constraint_set flags + level_idc
    // constraint_set0_flag (bit 7 of byte 1) = 1 means "Baseline compatible"
    //   - Baseline (66) with constraint_set0=1 => Constrained Baseline (42e0xx)
    //   - Main (77) with constraint_set0=1 => declares Baseline-compatible (4de0xx)
    //   - High (100) with constraint_set0=1 => declares Baseline-compatible (64e0xx)
    // The xHome console matches offered payload types on profile_idc + constraint flags
    // and IGNORES the level byte. Our old Main entry "4de0xx" set constraint_set0=1,
    // so the console treated it as Baseline-compatible and fell back to our Baseline PT.
    // Chrome offers Main as "4d00xx" (no constraint flags); the console answers
    // "4d002a" (Main, level 4.2) for that PT and offers NO High PT at all.
    match (video_h264_profile, video_fps > 30) {
        (H264Profile::Baseline, false) => "42e01f", // Constrained Baseline, level 3.1
        (H264Profile::Baseline, true) => "42e020",  // Constrained Baseline, level 4.0
        (H264Profile::Main, false) => "4d001f",     // Main, level 3.1 (no constraint flags)
        (H264Profile::Main, true) => "4d0020",      // Main, level 4.0 (no constraint flags)
        (H264Profile::High, false) => "64001f",     // High, level 3.1
        (H264Profile::High, true) => "640020",      // High, level 4.0
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
        assert_eq!(h264_profile_level_id(30, H264Profile::Main), "4d001f");
        assert_eq!(h264_profile_level_id(60, H264Profile::Main), "4d0020");
        assert_eq!(h264_profile_level_id(30, H264Profile::High), "64001f");
        assert_eq!(h264_profile_level_id(60, H264Profile::High), "640020");
    }
}
