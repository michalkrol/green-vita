/// Finds the extmap id the ANSWER negotiated for `uri` (e.g. transport-cc), so
/// inbound RTP header extensions can be parsed without crate internals.
/// VERIFIED 2026-10-30: against our own last-offer.sdp this returns None
/// (we offer no extmaps); callers unwrap_or(0), which read downstream as
/// "negotiated id 0" and masked "never negotiated" as "negotiated but
/// ignored". Becomes meaningful once 2a puts a=extmap lines in the offer.
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

/// Removes advisory `a=framerate` attributes from the video section (browser
/// parity: browsers never send this line). The console ignores fps declarations
/// at the app layer yet emits ~58-62 AUs/s regardless; if it paces egress by the
/// NEGOTIATED framerate instead, advertising 30 against 60 fps production grows
/// its pre-stamp send queue linearly — the observed drift signature.
pub(super) fn strip_video_fps(sdp: &str) -> String {
    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = sdp.ends_with('\n');
    let lines = sdp
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    let output = lines
        .into_iter()
        .filter(|line| !line.starts_with("a=framerate:"))
        .collect::<Vec<_>>();

    let mut result = output.join(newline);
    if trailing_newline {
        result.push_str(newline);
    }
    result
}

/// S5b: parses the ANSWER SDP for negotiated RTX payload types and their
/// associated primary H.264 payload types (via `a=fmtp:<rtx-pt> apt=<pt>`).
/// Returns RTX PT -> primary PT. The console may renumber payload types, so
/// the answer is the source of truth (ours echoes 112/apt=102 — parse anyway).
pub fn extract_negotiated_rtx_mapping(sdp: &str) -> std::collections::HashMap<u8, u8> {
    use std::collections::HashMap;
    let mut rtx_map = HashMap::new();
    let mut rtx_apt: HashMap<u8, u8> = HashMap::new();
    let mut in_video = false;

    for line in sdp.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("m=") {
            in_video = line.starts_with("m=video ");
            continue;
        }
        if !in_video {
            continue;
        }
        if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            if let Some((pt_str, encoding)) = rest.split_once(' ') {
                if let Ok(pt) = pt_str.parse::<u8>() {
                    if encoding.to_ascii_lowercase().starts_with("rtx/") {
                        rtx_map.insert(pt, 0); // placeholder, filled from fmtp below
                    }
                }
            }
        }
        if let Some(rest) = line.strip_prefix("a=fmtp:") {
            if let Some((pt_str, fmtp)) = rest.split_once(' ') {
                if let Ok(pt) = pt_str.parse::<u8>() {
                    for param in fmtp.split(';') {
                        let param = param.trim();
                        if let Some(apt_str) = param.strip_prefix("apt=") {
                            if let Ok(apt) = apt_str.parse::<u8>() {
                                rtx_apt.insert(pt, apt);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut result = HashMap::new();
    for (rtx_pt, _) in rtx_map {
        if let Some(&h264_pt) = rtx_apt.get(&rtx_pt) {
            result.insert(rtx_pt, h264_pt);
        }
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

/// Bitrate cap injected into the offer's video section, in kbps.
/// Overridden by settings-based cap. Kept as default for non-settings paths.
pub(super) const VIDEO_BITRATE_CAP_KBPS: u32 = 15_000;

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
    fn extracts_rtx_mapping_from_real_answer_sdp() {
        // Gold fixture: REAL answer shape from this console (S5a session,
        // byte-same structure as the 0.2.5 hardware capture): CRLF endings,
        // m=video ... 102 112, rtpmap:112 rtx/90000, fmtp:112 apt=102,
        // ssrc-group:FID primary+rtx.
        let real_answer = concat!(
            "v=0\r\n",
            "o=- 9008743018967891990 2 IN IP4 127.0.0.1\r\n",
            "s=-\r\n",
            "t=0 0\r\n",
            "a=group:BUNDLE 0 1 2\r\n",
            "a=extmap-allow-mixed\r\n",
            "a=msid-semantic: WMS 0 1\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102 112\r\n",
            "c=IN IP4 0.0.0.0\r\n",
            "a=rtcp:9 IN IP4 0.0.0.0\r\n",
            "a=ice-ufrag:AUW6\r\n",
            "a=ice-pwd:QZ8/E5Bo73HjU1HvjBV/Cdqb\r\n",
            "a=ice-options:trickle renomination\r\n",
            "a=fingerprint:sha-256 DA:84:7B:39:10:C4:C0:AE:F0:87:DD:42:83:C3:21:07:5B:D4:C6:25:1B:FB:65:22:44:24:17:88:B6:C0:A1:08\r\n",
            "a=setup:passive\r\n",
            "a=mid:0\r\n",
            "a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n",
            "a=sendonly\r\n",
            "a=msid:1 39ea3f26-954c-47d1-ac79-0eb6ce3b2a41\r\n",
            "a=rtcp-mux\r\n",
            "a=rtcp-rsize\r\n",
            "a=rtpmap:102 H264/90000\r\n",
            "a=rtcp-fb:102 goog-remb\r\n",
            "a=rtcp-fb:102 transport-cc\r\n",
            "a=rtcp-fb:102 ccm fir\r\n",
            "a=rtcp-fb:102 nack\r\n",
            "a=rtcp-fb:102 nack pli\r\n",
            "a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e02a\r\n",
            "a=rtpmap:112 rtx/90000\r\n",
            "a=fmtp:112 apt=102\r\n",
            "a=ssrc-group:FID 868043629 4263802149\r\n",
            "a=ssrc:868043629 cname:xmzYc+Xm4zTiygSJ\r\n",
            "a=ssrc:4263802149 cname:xmzYc+Xm4zTiygSJ\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
            "c=IN IP4 0.0.0.0\r\n",
            "a=rtcp:9 IN IP4 0.0.0.0\r\n",
            "a=ice-ufrag:AUW6\r\n",
            "a=ice-pwd:QZ8/E5Bo73HjU1HvjBV/Cdqb\r\n",
            "a=ice-options:trickle renomination\r\n",
            "a=fingerprint:sha-256 DA:84:7B:39:10:C4:C0:AE:F0:87:DD:42:83:C3:21:07:5B:D4:C6:25:1B:FB:65:22:44:24:17:88:B6:C0:A1:08\r\n",
            "a=setup:passive\r\n",
            "a=mid:1\r\n",
            "a=sendonly\r\n",
            "a=msid:0 764a2bcf-546f-4c0a-a0d1-4a1ae07c9d4aa\r\n",
            "a=rtcp-mux\r\n",
            "a=rtcp-rsize\r\n",
            "a=rtpmap:111 opus/48000/2\r\n",
            "a=ssrc:1207321385 cname:xmzYc+Xm4zTiygSJ\r\n",
            "m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n",
            "c=IN IP4 0.0.0.0\r\n",
            "a=ice-ufrag:AUW6\r\n",
            "a=ice-pwd:QZ8/E5Bo73HjU1HvjBV/Cdqb\r\n",
            "a=ice-options:trickle renomination\r\n",
            "a=fingerprint:sha-256 DA:84:7B:39:10:C4:C0:AE:F0:87:DD:42:83:C3:21:07:5B:D4:C6:25:1B:FB:65:22:44:24:17:88:B6:C0:A1:08\r\n",
            "a=setup:passive\r\n",
            "a=mid:2\r\n",
            "a=sctp-port:5000\r\n",
        );
        let rtx_map = extract_negotiated_rtx_mapping(real_answer);
        assert_eq!(rtx_map.len(), 1, "got {rtx_map:?}");
        assert_eq!(rtx_map.get(&112), Some(&102), "got {rtx_map:?}");
    }

    #[test]
    fn leaves_sdp_without_h264_untouched() {
        let offer = "v=0\nm=audio 9 UDP/TLS/RTP/SAVPF 111\n";
        assert_eq!(cap_video_bitrate(offer, 8_000), offer);
    }
}

#[cfg(test)]
mod extmap_tests {
    use super::extract_extmap_id;

    /// 2a (2026-10-30): extractor proven against real answer-style lines
    /// (pre-2a it never ran against a single extmap line — our offers had
    /// none, so every call returned None and unwrap_or(0) masked it).
    #[test]
    fn extracts_negotiated_ids_from_answer_lines() {
        let sdp = "m=video 9 UDP/TLS/RTP/SAVPF 102\r\n\
            a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n\
            a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r\n\
            a=extmap:7 http://www.webrtc.org/experiments/rtp-hdrext/video-timing\r\n";
        assert_eq!(
            extract_extmap_id(sdp, "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01"),
            Some(3)
        );
        assert_eq!(
            extract_extmap_id(sdp, "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time"),
            Some(2)
        );
        assert_eq!(
            extract_extmap_id(sdp, "http://www.webrtc.org/experiments/rtp-hdrext/video-timing"),
            Some(7)
        );
        assert_eq!(
            extract_extmap_id(sdp, "http://www.webrtc.org/experiments/rtp-hdrext/color-space"),
            None
        );
        // Pre-2a shape: no extmap lines at all -> None (NOT id 0).
        assert_eq!(extract_extmap_id("m=video 9 UDP/TLS/RTP/SAVPF 102\r\n", "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time"), None);
    }
}
