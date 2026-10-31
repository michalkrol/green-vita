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
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpHeaderExtensionCapability,
    RtpCodecKind,
};

/// Inbound header-extension URIs offered since 2a (2026-10-30). The old note
/// here — "the extension is no longer offered since the console never
/// annotates toward us at useful density" — was refuted by the chrome dumps:
/// the same console annotates Chrome's negotiated extmaps densely enough for
/// 506-1290 TWCC feedback/session. Pre-2a we negotiated NO extensions at all,
/// so annotation was structurally impossible (and extract_extmap_id's
/// unwrap_or(0) masked "never negotiated" as "id 0"). Answer ids are stored
/// per-URI in rtp.rs; never hardcoded (rtc assigns ids from registration
/// order — the old vt census hardcoding id 7 could never match).
pub(super) const TWCC_URI: &str =
    "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";
pub(super) const ABS_SEND_TIME_URI: &str =
    "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time";
pub(super) const VIDEO_TIMING_URI: &str =
    "http://www.webrtc.org/experiments/rtp-hdrext/video-timing";

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
    // Offer: H264 video + Opus audio; feedback = goog-remb, ccm fir, generic
    // nack + nack pli, transport-cc; one RTX pair (2a + S5a, 2026-10-31).
    // ARCHAEOLOGY (git -S + upstream, 2026-10-31): 03872f5's original note
    // said RTX/FEC/profiles/extensions proved INERT on the drift line; the
    // REGRESSIVE items were #21 (annotation gating — refuted by 2a) and #35
    // (browser-shape SDP mirror = cc35824: drop b=AS + mirror Chrome's
    // extmaps; upstream issue now 404s). Our 690f0c7 rewording had collapsed
    // that distinction. S5a adds nack+rtx ONLY: b=AS stays, single profile
    // stays, no FEC, no extra extension mirrors.
    // 2a also re-adds header extensions: the previous "No header extensions"
    // rationale rested on "the console never annotates toward us", which the
    // chrome dumps refute (same console, dense annotation once negotiated),
    // and on "#21 annotation gating" experiments made before any extension
    // was ever negotiated here. The absences had measurable cost: vt census
    // dead (vt_exit_count=0 every gate session), abs-send-time unparsed
    // (2b blocked), twcc annotation impossible (2c blocked).

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
                    "level-asymmetry-allowed=0;packetization-mode=1;profile-level-id={}",
                    h264_profile_level_id(video_fps, video_h264_profile)
                ),
                rtcp_feedback: vec![
                    RTCPFeedback { typ: "goog-remb".to_owned(), parameter: "".to_owned() },
                    RTCPFeedback { typ: "ccm".to_owned(), parameter: "fir".to_owned() },
                    RTCPFeedback { typ: "nack".to_owned(), parameter: "".to_owned() },
                    RTCPFeedback { typ: "nack".to_owned(), parameter: "pli".to_owned() },
                    RTCPFeedback { typ: "transport-cc".to_owned(), parameter: "".to_owned() },
                ],
            },
            payload_type: 102,
        },
        RtpCodecKind::Video,
    )?;

    // S5a (2026-10-31): RTX pair for the H264 above — PT 112 + apt=102, the
    // exact pairing archive/0.2.5 hardware-proved (rtx_read_rewritten=44,
    // never reverted on that branch). Negotiation-only: no NACK sender yet,
    // so no retransmission traffic (inert per HANDOFF §3.11 + drift-era
    // "proved inert"; S5b ports 0.2.5's NACK+RTX trio to light it up).
    media_engine.register_codec(
        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_RTX.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: "apt=102".to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: 112,
        },
        RtpCodecKind::Video,
    )?;

    // 2a: inbound header extensions. Ids come back in the ANSWER and are
    // stored per-URI in worker.rs exchange_sdp — never hardcoded.
    for uri in [TWCC_URI, ABS_SEND_TIME_URI, VIDEO_TIMING_URI] {
        media_engine.register_header_extension(
            RTCRtpHeaderExtensionCapability { uri: uri.to_owned() },
            RtpCodecKind::Video,
            None,
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

    #[test]
    fn s5a_registers_nack_and_rtx_without_error() {
        use super::register_vita_codecs;
        use rtc::peer_connection::configuration::media_engine::MediaEngine;

        // This rtc build exposes no public codec introspection
        // (get_codecs_by_kind is private), so host-side we prove the
        // registration path ACCEPTS the S5a additions (video/rtx mime,
        // PT 112, generic-nack feedback entry). Offer/answer CONTENT is
        // verified from the device's last-offer/answer.sdp after flash
        // (the 2a handshake method): a=rtcp-fb:102 nack,
        // a=rtpmap:112 video/rtx, a=fmtp:112 apt=102 + console echo.
        let mut engine = MediaEngine::default();
        register_vita_codecs(&mut engine, 60, H264Profile::Baseline).unwrap();
    }
}
