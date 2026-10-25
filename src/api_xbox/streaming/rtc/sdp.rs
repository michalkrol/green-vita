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
use super::peer::TWCC_URI;

/// Injects the two required header-ext map lines into the VIDEO m-section of an SDP offer:
///   a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time
///   a=extmap:7 http://www.webrtc.org/experiments/rtp-hdrext/video-timing
/// All other lines (audio, b=AS, codec lines, etc.) are left untouched.
/// Build-time feature flag: offer abs-send-time(2) + video-timing(7) extmaps (Phase 1.1).
/// Set false to restore the historical zero-extmap offer for A/B comparisons.
/// 
/// The actual flag value is now defined in `crate::flags::OFFER_VIDEO_EXTMAPS`.
pub(super) fn inject_video_extmaps(sdp: &str) -> String {
    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = sdp.ends_with('\n');
    let lines: Vec<String> = sdp.lines().map(|l| l.trim_end_matches('\r').to_owned()).collect();
    let mut output = Vec::with_capacity(lines.len() + 2);
    let mut injected = false;

    for line in &lines {
        output.push(line.clone());
        // After we encounter the video m-line, inject the two extmap lines once.
        if !injected && line.starts_with("m=video ") {
            injected = true;
            // Insert extmap lines after the m=video line
            output.push("a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time".to_owned());
            output.push("a=extmap:7 http://www.webrtc.org/experiments/rtp-hdrext/video-timing".to_owned());
        }
    }

    let mut result = output.join(newline);
    if trailing_newline && !result.ends_with(newline) {
        result.push_str(newline);
    }
    result
}

/// Injects the transport-wide-cc extmap and RTCP-FB lines into the VIDEO m-section of an SDP offer.
/// When the TWCC extension is offered, the answer should echo back extmap:3 with the
/// draft-holmer URI, and the video m-section should include `a=rtcp-fb:102 transport-cc`.
/// All other lines (audio, b=AS, codec lines, etc.) are left untouched.
/// Build-time feature flag: offer extmap:3 (transport-wide-cc draft-holmer URI) + rtcp-fb (Phase 1.2).
/// Set false to restore the historical zero-extmap offer for A/B comparisons.
///
/// The actual flag value is now defined in `crate::flags::OFFER_TWCC_EXTMAP`.
pub(crate) fn inject_twcc_extmap(sdp: &str) -> String {
    // Idempotency guard: never inject twice (a second ICE-offer re-use would duplicate).
    if sdp.contains(TWCC_URI) {
        return sdp.to_owned();
    }
    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = sdp.ends_with('\n');
    let lines: Vec<String> = sdp.lines().map(|l| l.trim_end_matches('\r').to_owned()).collect();
    let mut output = Vec::with_capacity(lines.len() + 2);
    let mut injected = false;

    for line in &lines {
        output.push(line.clone());
        // After we encounter the video m-line, inject the TWCC extmap lines once.
        if !injected && line.starts_with("m=video ") {
            injected = true;
            // Insert extmap:3 line after the m=video line
            output.push(format!("a=extmap:3 {}", TWCC_URI));
            // Insert rtcp-fb line after the m=video line
            output.push("a=rtcp-fb:102 transport-cc".to_owned());
        }
    }

    let mut result = output.join(newline);
    if trailing_newline && !result.ends_with(newline) {
        result.push_str(newline);
    }
    result
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
pub(super) fn cap_video_bitrate(sdp: &str, max_kbps: u32) -> String {
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
                // max_kbps == 0 means the user picked "no cap": leave the offer without
                // bandwidth lines instead of asking for an impossible zero bitrate.
                if max_kbps > 0 {
                    output.push(format!("b=AS:{max_kbps}"));
                    output.push(format!("b=TIAS:{}", u64::from(max_kbps) * 1000));
                }
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
    fn offer_injects_abs_send_time_and_video_timing_extmaps() {
        let offer = concat!(
            "v=0
",
            "m=video 9 UDP/TLS/RTP/SAVPF 102
",
            "a=rtpmap:102 H264/90000
",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111
",
            "a=rtpmap:111 opus/48000/2
",
        );

        let with_extmaps = inject_video_extmaps(offer);

        // Verify both extmap lines are present in the video section
        assert!(
            with_extmaps.contains("a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time"),
            "Offer should contain extmap:2 abs-send-time URI"
        );
        assert!(
            with_extmaps.contains("a=extmap:7 http://www.webrtc.org/experiments/rtp-hdrext/video-timing"),
            "Offer should contain extmap:7 video-timing URI"
        );

        // Verify correct URIs
        assert!(
            with_extmaps.contains("http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time"),
            "Offer should use canonical abs-send-time URI"
        );
        assert!(
            with_extmaps.contains("http://www.webrtc.org/experiments/rtp-hdrext/video-timing"),
            "Offer should use canonical video-timing URI"
        );
    }

    #[test]
    fn offer_contains_no_transport_cc_extmap_after_injection() {
        let offer = concat!(
            "v=0
",
            "m=video 9 UDP/TLS/RTP/SAVPF 102
",
            "a=rtpmap:102 H264/90000
",
            "b=AS:2500
",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111
",
            "a=rtpmap:111 opus/48000/2
",
        );

        let with_extmaps = inject_video_extmaps(offer);

        // Should NOT contain transport-cc related extmaps
        assert!(
            !with_extmaps.contains("extmap:3"),
            "Offer should not contain transport-cc extmap after injection"
        );
        assert!(
            !with_extmaps.contains("a=rtcp-fb:"),
            "Offer should not contain rtcp-fb lines after injection"
        );
        // b=AS should still be present
        assert!(
            with_extmaps.contains("b=AS:2500"),
            "b=AS bandwidth line should be preserved after extmap injection"
        );
    }

    #[test]
    fn offer_other_content_unchanged_by_extmap_injection() {
        let offer = concat!(
            "v=0
",
            "m=video 9 UDP/TLS/RTP/SAVPF 102
",
            "a=rtpmap:102 H264/90000
",
            "b=AS:2500
",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111
",
            "a=rtpmap:111 opus/48000/2
",
        );

        let with_extmaps = inject_video_extmaps(offer);

        // b=AS should still be present and in the right position (before audio m-line)
        assert!(
            with_extmaps.contains("b=AS:2500"),
            "b=AS should still be in video section after injection"
        );
        // Audio section should be untouched
        assert!(
            with_extmaps.contains("a=rtpmap:111 opus/48000/2"),
            "Audio rtpmap should be unchanged"
        );
    }

    #[test]
    fn offer_twcc_injection_adds_extmap3_and_transport_cc_fb() {
        let offer = concat!(
            "v=0
",
            "m=video 9 UDP/TLS/RTP/SAVPF 102
",
            "a=rtpmap:102 H264/90000
",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111
",
            "a=rtpmap:111 opus/48000/2
",
        );

        let with_twcc = inject_twcc_extmap(offer);

        // Verify extmap:3 line is present with exact TWCC URI
        assert!(
            with_twcc.contains("a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01"),
            "Offer should contain extmap:3 transport-wide-cc URI"
        );

        // Verify rtcp-fb:102 line is present
        assert!(
            with_twcc.contains("a=rtcp-fb:102 transport-cc"),
            "Offer should contain rtcp-fb:102 transport-cc line"
        );

        // Verify m=video is still intact
        assert!(
            with_twcc.contains("m=video 9 UDP/TLS/RTP/SAVPF 102"),
            "m=video line should be preserved after injection"
        );

        // Verify idempotency: a second call produces the same output
        let second_call = inject_twcc_extmap(&with_twcc);
        assert_eq!(with_twcc, second_call, "Second call to inject_twcc_extmap should be idempotent");
    }
}
