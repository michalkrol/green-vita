/// Finds the extmap id the ANSWER negotiated for `uri` (e.g. transport-cc), so
/// inbound RTP header extensions can be parsed without crate internals.
pub(super) fn extract_extmap_id(sdp: &str, uri: &str) -> Option<u8> {
    for line in sdp.lines() {
        let Some(rest) = line.strip_prefix("a=extmap:") else {
            continue;
        };
        let id = rest
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u8>()
            .ok()?;
        if line.to_ascii_lowercase().contains(&uri.to_ascii_lowercase()) {
            return Some(id);
        }
    }
    None
}

/// Adds the advisory receive-frame-rate attribute to the H.264 video section.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn request_video_fps(sdp: &str, video_fps: u32) -> String {
    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = sdp.ends_with('\n');
    let lines = sdp
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(lines.len() + 1);
    let mut in_video = false;
    let mut video_has_h264 = false;

    for line in lines {
        if line.starts_with("m=") {
            if in_video && video_has_h264 {
                output.push(format!("a=framerate:{video_fps}"));
            }
            in_video = line.starts_with("m=video ");
            video_has_h264 = false;
        }
        if in_video && line.to_ascii_lowercase().contains(" h264/") {
            video_has_h264 = true;
        }
        if !(in_video && line.starts_with("a=framerate:")) {
            output.push(line.to_owned());
        }
    }
    if in_video && video_has_h264 {
        output.push(format!("a=framerate:{video_fps}"));
    }

    let mut result = output.join(newline);
    if trailing_newline {
        result.push_str(newline);
    }
    result
}

/// Injects `b=AS`/`b=TIAS` bandwidth lines into the H.264 video media section.
/// Pass 0 to leave the SDP uncapped (no bandwidth lines injected).
pub(super) fn cap_video_bitrate(sdp: &str, max_kbps: u32) -> String {
    if max_kbps == 0 {
        return sdp.to_owned();
    }
    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = sdp.ends_with('\n');
    let lines = sdp
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(lines.len() + 2);
    let mut in_video = false;
    let mut video_has_h264 = false;
    let mut injected = false;

    for line in lines {
        if line.starts_with("m=") {
            if in_video && video_has_h264 && !injected {
                output.push(format!("b=AS:{max_kbps}"));
                output.push(format!("b=TIAS:{}", u64::from(max_kbps) * 1000));
                injected = true;
            }
            in_video = line.starts_with("m=video ");
            video_has_h264 = false;
        }
        if in_video && line.to_ascii_lowercase().contains(" h264/") {
            video_has_h264 = true;
        }
        // Drop stale bandwidth lines from the capped section.
        if in_video && (line.starts_with("b=AS:") || line.starts_with("b=TIAS:")) {
            continue;
        }
        output.push(line.to_owned());
    }

    let mut result = output.join(newline);
    if trailing_newline {
        result.push_str(newline);
    }
    result
}

/// Injects `transport-wide-cc` feedback declaration and both extmap URIs into the
/// video section.  The console annotates every RTP packet with transport sequence
/// numbers only when explicitly requested in the SDP offer; without these lines the
/// console falls back to ~1/s sparse probing, starving client-side TWCC feedback.
pub(super) fn enable_transport_cc(sdp: &str) -> String {
    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = sdp.ends_with('\n');
    let lines = sdp
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(lines.len() + 3);

    let mut in_video = false;
    let mut video_h264_pt = String::new();
    let mut extmap_injected = false;
    let mut fb_injected = false;

    for line in lines {
        // Detect which payload type H264 uses, and insert extmap + rtcp-fb lines
        // right before the first media-level line of the video section.
        if line.starts_with("m=") {
            if in_video && !extmap_injected {
                output.push(format!("a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01"));
                output.push(format!("a=extmap:4 http://www.webrtc.org/experiments/rtp-hdrext/transport-wide-cc-02"));
                extmap_injected = true;
            }
            in_video = line.starts_with("m=video ");
        }
        if in_video && line.starts_with("a=rtpmap:") && line.to_ascii_lowercase().contains("h264") {
            // Extract payload type from "a=rtpmap:102 H264/90000"
            if let Some(pt) = line
                .strip_prefix("a=rtpmap:")
                .and_then(|rest| rest.split_whitespace().next())
            {
                video_h264_pt = pt.to_owned();
            }
        }
        // Inject rtcp-fb line AFTER the last existing rtcp-fb line for the same PT
        // (or before the next non-rtcp-fb line).
        if in_video && line.starts_with(&format!("a=rtcp-fb:{video_h264_pt} ")) {
            // Let the existing line through, then inject ours right after.
            output.push(line.to_owned());
            if !fb_injected {
                output.push(format!("a=rtcp-fb:{video_h264_pt} transport-cc"));
                fb_injected = true;
            }
            continue;
        }
        output.push(line.to_owned());
    }

    // If video section never ended (no `m=` transition) and we missed injection,
    // inject at the end.
    if in_video && !extmap_injected {
        output.push(format!("a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01"));
        output.push(format!("a=extmap:4 http://www.webrtc.org/experiments/rtp-hdrext/transport-wide-cc-02"));
    }
    if in_video && !fb_injected && !video_h264_pt.is_empty() {
        output.push(format!("a=rtcp-fb:{video_h264_pt} transport-cc"));
    }

    let mut result = output.join(newline);
    if trailing_newline {
        result.push_str(newline);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_thirty_fps_only_in_the_h264_video_section() {
        let offer = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102\r\n",
            "a=rtpmap:102 H264/90000\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
            "a=rtpmap:111 opus/48000/2\r\n",
        );

        let constrained = request_video_fps(offer, 30);
        assert_eq!(constrained.matches("a=framerate:30").count(), 1);
        assert!(constrained.find("a=framerate:30").unwrap() < constrained.find("m=audio").unwrap());
    }

    #[test]
    fn replaces_existing_frame_rate_when_unlocked() {
        let offer = concat!(
            "v=0\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102\n",
            "a=rtpmap:102 H264/90000\n",
            "a=framerate:30\n",
        );

        let unlocked = request_video_fps(offer, 60);
        assert!(!unlocked.contains("a=framerate:30"));
        assert_eq!(unlocked.matches("a=framerate:60").count(), 1);
    }

    #[test]
    fn injects_bandwidth_cap_into_the_h264_video_section() {
        let offer = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102\r\n",
            "a=rtpmap:102 H264/90000\r\n",
            "b=AS:2500\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
        );

        let capped = cap_video_bitrate(offer, 8_000);
        assert_eq!(capped.matches("b=AS:8000").count(), 1);
        assert_eq!(capped.matches("b=TIAS:8000000").count(), 1);
        assert!(!capped.contains("b=AS:2500"));
        let video_end = capped.find("m=audio").unwrap();
        assert!(capped.find("b=AS:8000").unwrap() < video_end);
        assert!(capped.find("b=TIAS:8000000").unwrap() < video_end);
    }

    #[test]
    fn leaves_sdp_without_h264_untouched() {
        let offer = "v=0\nm=audio 9 UDP/TLS/RTP/SAVPF 111\n";
        assert_eq!(cap_video_bitrate(offer, 8_000), offer);
    }

    #[test]
    fn merges_constraints_without_overriding_the_negotiated_profile() {
        let offer = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102 103\r\n",
            "a=rtpmap:102 H264/90000\r\n",
            "a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=4de01f\r\n",
            "a=rtpmap:103 H264/90000\r\n",
            "a=fmtp:103 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
            "a=rtpmap:111 opus/48000/2\r\n",
        );

        let constrained = constrain_h264_codec(offer, 30);

        // Exactly one fmtp line for the primary payload type (no duplicate).
        assert_eq!(constrained.matches("a=fmtp:102 ").count(), 1, "{constrained}");

        let fmtp102 = constrained
            .lines()
            .find(|line| line.trim_end_matches('\r').starts_with("a=fmtp:102 "))
            .expect("fmtp line for 102");
        // The negotiated Main profile survives, and the Baseline override is gone.
        assert!(fmtp102.contains("profile-level-id=4de01f"), "{fmtp102}");
        assert!(!fmtp102.contains("42e01f"), "{fmtp102}");
        assert!(fmtp102.contains("max-fs=8160"), "{fmtp102}");
        // The requested frame rate is declared as the max-fr cap.
        assert!(fmtp102.contains("max-fr=30"), "{fmtp102}");
        // max-qp is merged in to cap the quantiser for dark-scene quality.
        assert!(fmtp102.contains("max-qp=35"), "{fmtp102}");
        // The fallback payload type is left untouched.
        assert!(
            constrained
                .contains("a=fmtp:103 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"),
            "{constrained}"
        );
    }

    #[test]
    fn does_not_duplicate_max_fs_when_already_present() {
        let offer = concat!(
            "v=0\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102\n",
            "a=rtpmap:102 H264/90000\n",
            "a=fmtp:102 profile-level-id=42e01f;packetization-mode=1;max-fr=30;max-fs=8160\n",
        );

        let constrained = constrain_h264_codec(offer, 30);
        assert_eq!(constrained.matches("max-fs=8160").count(), 1, "{constrained}");
        assert_eq!(constrained.matches("max-fr=30").count(), 1, "{constrained}");
        assert_eq!(constrained.matches("a=fmtp:102 ").count(), 1, "{constrained}");
    }

    #[test]
    fn does_not_duplicate_max_qp_when_already_present() {
        let offer = concat!(
            "v=0\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102\n",
            "a=rtpmap:102 H264/90000\n",
            "a=fmtp:102 profile-level-id=42e01f;packetization-mode=1;max-fr=30;max-fs=8160;max-qp=40\n",
        );

        let constrained = constrain_h264_codec(offer, 30);
        assert_eq!(constrained.matches("max-qp=40").count(), 1, "{constrained}");
        assert_eq!(constrained.matches("max-qp=35").count(), 0, "{constrained}");
        assert_eq!(constrained.matches("a=fmtp:102 ").count(), 1, "{constrained}");
    }

    #[test]
    fn synthesises_fmtp_only_when_the_payload_type_has_none() {
        let offer = "v=0\nm=video 9 UDP/TLS/RTP/SAVPF 102\na=rtpmap:102 H264/90000\n";
        let constrained = constrain_h264_codec(offer, 30);
        assert_eq!(constrained.matches("a=fmtp:102 ").count(), 1, "{constrained}");
        assert!(constrained.contains("profile-level-id=42e01f"), "{constrained}");
        assert!(constrained.contains("max-fs=8160"), "{constrained}");
        assert!(constrained.contains("max-fr=30"), "{constrained}");
        assert!(constrained.contains("max-qp=35"), "{constrained}");
    }

    #[test]
    fn leaves_a_stream_without_h264_untouched() {
        let offer = "v=0\nm=audio 9 UDP/TLS/RTP/SAVPF 111\na=rtpmap:111 opus/48000/2\n";
        assert_eq!(constrain_h264_codec(offer, 30), offer);
    }

    #[test]
    fn max_fr_follows_the_requested_frame_rate() {
        let offer = concat!(
            "v=0\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102\n",
            "a=rtpmap:102 H264/90000\n",
            "a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e020\n",
        );

        let at_60 = constrain_h264_codec(offer, 60);
        assert!(at_60.contains("max-fr=60"), "{at_60}");
        assert!(!at_60.contains("max-fr=30"), "{at_60}");

        let at_30 = constrain_h264_codec(offer, 30);
        assert!(at_30.contains("max-fr=30"), "{at_30}");
    }

    #[test]
    fn preserves_rtx_payload_types_and_fmtp() {
        let offer = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102 112 103 113\r\n",
            "a=rtpmap:102 H264/90000\r\n",
            "a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=4d0020\r\n",
            "a=rtcp-fb:102 goog-remb\r\n",
            "a=rtcp-fb:102 ccm fir\r\n",
            "a=rtcp-fb:102 nack\r\n",
            "a=rtcp-fb:102 nack pli\r\n",
            "a=rtpmap:112 rtx/90000\r\n",
            "a=fmtp:112 apt=102\r\n",
            "a=rtpmap:103 H264/90000\r\n",
            "a=fmtp:103 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n",
            "a=rtcp-fb:103 goog-remb\r\n",
            "a=rtcp-fb:103 ccm fir\r\n",
            "a=rtcp-fb:103 nack\r\n",
            "a=rtcp-fb:103 nack pli\r\n",
            "a=rtpmap:113 rtx/90000\r\n",
            "a=fmtp:113 apt=103\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
            "a=rtpmap:111 opus/48000/2\r\n",
        );

        let constrained = constrain_h264_codec(offer, 30);

        // H264 PT 102 fmtp should be merged with constraints
        let fmtp102 = constrained.lines().find(|l| l.starts_with("a=fmtp:102 ")).expect("fmtp 102");
        assert!(fmtp102.contains("profile-level-id=4d0020"), "Negotiated profile preserved: {fmtp102}");
        assert!(fmtp102.contains("max-fr=30"), "max-fr added: {fmtp102}");
        assert!(fmtp102.contains("max-fs=8160"), "max-fs added: {fmtp102}");
        assert!(fmtp102.contains("max-qp=35"), "max-qp added: {fmtp102}");
        assert_eq!(constrained.matches("a=fmtp:102 ").count(), 1, "No duplicate fmtp 102");

        // RTX PT 112 should be untouched
        assert!(constrained.contains("a=rtpmap:112 rtx/90000"), "RTX rtpmap 112 preserved");
        assert!(constrained.contains("a=fmtp:112 apt=102"), "RTX fmtp 112 preserved");
        assert_eq!(constrained.matches("a=fmtp:112 ").count(), 1, "Exactly one fmtp 112");

        // H264 PT 103 fmtp should be untouched (fallback)
        assert!(constrained.contains("a=fmtp:103 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"), "Fallback H264 103 untouched");

        // RTX PT 113 should be untouched
        assert!(constrained.contains("a=rtpmap:113 rtx/90000"), "RTX rtpmap 113 preserved");
        assert!(constrained.contains("a=fmtp:113 apt=103"), "RTX fmtp 113 preserved");
        assert_eq!(constrained.matches("a=fmtp:113 ").count(), 1, "Exactly one fmtp 113");

        // Generic NACK feedback lines should be preserved
        assert!(constrained.contains("a=rtcp-fb:102 nack"), "Generic NACK for 102 preserved");
        assert!(constrained.contains("a=rtcp-fb:102 nack pli"), "NACK PLI for 102 preserved");
        assert!(constrained.contains("a=rtcp-fb:103 nack"), "Generic NACK for 103 preserved");
        assert!(constrained.contains("a=rtcp-fb:103 nack pli"), "NACK PLI for 103 preserved");
    }

    #[test]
    fn preserves_rtx_and_nack_through_cap_video_bitrate() {
        let offer = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102 112\r\n",
            "a=rtpmap:102 H264/90000\r\n",
            "a=fmtp:102 profile-level-id=4d0020\r\n",
            "a=rtcp-fb:102 nack\r\n",
            "a=rtpmap:112 rtx/90000\r\n",
            "a=fmtp:112 apt=102\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
            "a=rtpmap:111 opus/48000/2\r\n",
        );

        let capped = cap_video_bitrate(offer, 5000);
        assert!(capped.contains("b=AS:5000"), "Bitrate cap injected");
        assert!(capped.contains("b=TIAS:5000000"), "TIAS cap injected");
        assert!(capped.contains("a=rtpmap:112 rtx/90000"), "RTX preserved through cap_video_bitrate");
        assert!(capped.contains("a=fmtp:112 apt=102"), "RTX fmtp preserved");
        assert!(capped.contains("a=rtcp-fb:102 nack"), "NACK feedback preserved");
    }

    #[test]
    fn preserves_rtx_and_nack_through_enable_transport_cc() {
        let offer = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102 112\r\n",
            "a=rtpmap:102 H264/90000\r\n",
            "a=fmtp:102 profile-level-id=4d0020\r\n",
            "a=rtcp-fb:102 nack\r\n",
            "a=rtcp-fb:102 nack pli\r\n",
            "a=rtpmap:112 rtx/90000\r\n",
            "a=fmtp:112 apt=102\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
            "a=rtpmap:111 opus/48000/2\r\n",
        );

        let with_twcc = enable_transport_cc(offer);
        assert!(with_twcc.contains("a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01"), "TWCC extmap injected");
        assert!(with_twcc.contains("a=rtcp-fb:102 transport-cc"), "transport-cc feedback injected for H264 PT");
        assert!(with_twcc.contains("a=rtpmap:112 rtx/90000"), "RTX preserved through enable_transport_cc");
        assert!(with_twcc.contains("a=fmtp:112 apt=102"), "RTX fmtp preserved");
        assert!(with_twcc.contains("a=rtcp-fb:102 nack"), "Generic NACK preserved");
        assert!(with_twcc.contains("a=rtcp-fb:102 nack pli"), "NACK PLI preserved");
    }

    #[test]
    fn preserves_rtx_and_nack_through_request_video_fps() {
        let offer = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102 112\r\n",
            "a=rtpmap:102 H264/90000\r\n",
            "a=fmtp:102 profile-level-id=4d0020\r\n",
            "a=rtcp-fb:102 nack\r\n",
            "a=rtpmap:112 rtx/90000\r\n",
            "a=fmtp:112 apt=102\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
            "a=rtpmap:111 opus/48000/2\r\n",
        );

        let with_fps = request_video_fps(offer, 60);
        assert!(with_fps.contains("a=framerate:60"), "Framerate injected");
        assert!(with_fps.contains("a=rtpmap:112 rtx/90000"), "RTX preserved through request_video_fps");
        assert!(with_fps.contains("a=fmtp:112 apt=102"), "RTX fmtp preserved");
        assert!(with_fps.contains("a=rtcp-fb:102 nack"), "NACK feedback preserved");
    }
}

/// Merges H.264 codec constraints into the video payload type's *existing*
/// `a=fmtp` line instead of appending a second one.
///
/// Appending a duplicate was a bug: two `a=fmtp` lines for a single payload type
/// is invalid SDP, and the appended line hardcoded `profile-level-id=42e01f`
/// (Constrained Baseline) after the negotiated one, so a negotiated Main/High
/// profile was silently overridden back to Baseline.  The negotiated
/// `profile-level-id`, `packetization-mode` and `level-asymmetry-allowed` are now
/// preserved and only `max-fr`/`max-fs`/`max-qp` are merged in.
///
/// `max-fr` follows the offered frame rate, which comes from the "Unlock video
/// FPS" setting (30 by default, 60 when unlocked), so this is intended as the
/// 30/60 frame-rate switch.  Per RFC 6184 section 8.1 `max-fr` is expressed in
/// whole frames per second.  However, the console ignores both `a=framerate`
/// and `max-fr`; measured `src_fps` (metrics log) averages 58–70 fps regardless
/// of the offer.  The previous expression (`video_fps * 90_000 / video_fps`)
/// always evaluated to 90_000, i.e. it declared no cap at all.
///
/// `max-qp=35` caps the maximum quantiser. Dark/flat regions are where a
/// rate-distortion optimised encoder picks the coarsest quantiser (the absolute
/// error there is small so it is cheap in MSE, yet blocking/banding is highly
/// visible to a human), so capping the maximum QP forces bits into exactly the
/// areas that look worst.
///
/// **Caveat**: the console has been measured to IGNORE most SDP hints we send
/// (`a=framerate`, `max-fr`, `b=AS`/`b=TIAS` — it answered with no `b=` line at
/// all and keeps encoding ~58–70 fps and overshooting the declared cap), so this
/// parameter may well be ignored too. It is still worth sending because RFC 6184
/// requires an implementation to ignore fmtp parameters it does not understand,
/// so the downside is bounded, and because the console's answer SDP mirrors the
/// profile we offer, showing it does read the fmtp it cares about.
pub(super) fn constrain_h264_codec(sdp: &str, video_fps: u32) -> String {
    let max_fr_param = format!("max-fr={video_fps}");
    const MAX_FS_PARAM: &str = "max-fs=8160";
    const MAX_QP_PARAM: &str = "max-qp=35";

    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = sdp.ends_with('\n');
    let lines = sdp
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(lines.len() + 1);

    let mut in_video = false;
    let mut video_h264_pt = String::new();
    let mut fmtp_merged = false;

    for line in lines {
        if line.starts_with("m=") {
            in_video = line.starts_with("m=video ");
            fmtp_merged = false;
        }
        if in_video && line.starts_with("a=rtpmap:") && line.to_ascii_lowercase().contains("h264") {
            if let Some(pt) = line
                .strip_prefix("a=rtpmap:")
                .and_then(|rest| rest.split_whitespace().next())
            {
                video_h264_pt = pt.to_owned();
            }
        }

        // Merge the constraint into this payload type's existing fmtp line.
        if in_video
            && !fmtp_merged
            && !video_h264_pt.is_empty()
            && line.starts_with(&format!("a=fmtp:{video_h264_pt} "))
        {
            let mut merged = line.to_owned();
            if !merged.contains("max-fr=") {
                merged.push(';');
                merged.push_str(&max_fr_param);
            }
            if !merged.contains("max-fs=") {
                merged.push(';');
                merged.push_str(MAX_FS_PARAM);
            }
            if !merged.contains("max-qp=") {
                merged.push(';');
                merged.push_str(MAX_QP_PARAM);
            }
            output.push(merged);
            fmtp_merged = true;
            continue;
        }

        // No fmtp line for this payload type: synthesise one.  Nothing to
        // preserve in that case, so fall back to Constrained Baseline.
        if in_video
            && !fmtp_merged
            && !video_h264_pt.is_empty()
            && !line.starts_with("a=fmtp:")
            && !line.starts_with("a=rtpmap:")
            && (line.starts_with("a=") || line.starts_with("b="))
        {
            output.push(format!(
                "a=fmtp:{video_h264_pt} profile-level-id=42e01f;packetization-mode=1;{max_fr_param};{MAX_FS_PARAM};{MAX_QP_PARAM}"
            ));
            fmtp_merged = true;
        }

        output.push(line.to_owned());
    }

    if in_video && !fmtp_merged && !video_h264_pt.is_empty() {
        output.push(format!(
            "a=fmtp:{video_h264_pt} profile-level-id=42e01f;packetization-mode=1;{max_fr_param};{MAX_FS_PARAM};{MAX_QP_PARAM}"
        ));
    }

    let mut result = output.join(newline);
    if trailing_newline {
        result.push_str(newline);
    }
    result
}
