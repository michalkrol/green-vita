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
}
