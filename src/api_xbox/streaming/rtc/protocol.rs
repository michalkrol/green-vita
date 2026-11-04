use crate::api::streaming::rtc::session::RtcSessionBackend;
use crate::api_xbox::streaming::control::channel::{self, HandshakeStage};
use crate::api_xbox::streaming::control::input::{InputQueue, PointerFrame};
use crate::streaming::input::{GamepadFrame, PointerEvent};
use crate::streaming::video::{STREAM_HEIGHT, STREAM_WIDTH};
use bytes::BytesMut;
use std::time::{Duration, Instant};
use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use rtc::data_channel::RTCDataChannelId;

/// Reference clients sample input on a 16 ms setTimeout (~62.5 Hz); the console
/// parses gamepad reports in the same process that runs its capture/encode
/// pipeline. Our old flood (~200-300 reports/s, event-driven) correlated with
/// video lag; zero input instead trips the console's ~60 s liveness watchdog
/// (session freezes) — reports must keep flowing even when state is unchanged.
pub(super) const INPUT_MIN_INTERVAL: Duration = Duration::from_millis(16);
/// Floor lever (Step 7): a CHANGED gamepad state bypasses the 16 ms cadence
/// (bounded by this small floor against pathological pump rates) — cuts the
/// fixed-cadence phase-alignment penalty (~8 ms average) to ~0. Unchanged
/// state keeps the 16 ms liveness keepalive (reference-client parity).
pub(super) const INPUT_MIN_INTERVAL_CHANGED: Duration = Duration::from_millis(4);

/// Pure throttle decision (unit-tested): `elapsed_since_last` = time since
/// the last input send (None = never sent). Unchanged state obeys the 16 ms
/// keepalive cadence; changed state obeys only the 4 ms floor.
fn input_throttled(elapsed_since_last: Option<Duration>, changed: bool) -> bool {
    let min = if changed {
        INPUT_MIN_INTERVAL_CHANGED
    } else {
        INPUT_MIN_INTERVAL
    };
    matches!(elapsed_since_last, Some(e) if e < min)
}

#[derive(Clone, Copy)]
pub(in crate::api_xbox::streaming) struct ChannelIds {
    pub control: RTCDataChannelId,
    pub input: RTCDataChannelId,
    pub message: RTCDataChannelId,
}

pub(super) struct XboxRtcProtocol {
    handshake_stage: HandshakeStage,
    input_queue: InputQueue,
    input_channel_ready: bool,
    warned_input_not_ready: bool,
    server_video_size: Option<(u32, u32)>,
    channel_ids: ChannelIds,
    video_fps: u32,
    flush_size_toggle: bool,
    last_input_sent_at: Option<Instant>,
    last_gamepad: Option<GamepadFrame>,
}

impl XboxRtcProtocol {
    pub(super) fn new(channel_ids: ChannelIds, video_fps: u32) -> Self {
        Self {
            handshake_stage: HandshakeStage::WaitingForChannels,
            input_queue: InputQueue::default(),
            input_channel_ready: false,
            warned_input_not_ready: false,
            server_video_size: None,
            channel_ids,
            video_fps,
            flush_size_toggle: false,
            last_input_sent_at: None,
            last_gamepad: None,
        }
    }

    fn handshake_label(&self) -> &'static str {
        match self.handshake_stage {
            HandshakeStage::WaitingForChannels => "wait-channels",
            HandshakeStage::WaitingForHandshakeAck => "wait-ack",
            HandshakeStage::Ready => "ready",
        }
    }

    fn send_gamepad_frame_inner(
        &mut self,
        peer: &mut RTCPeerConnection,
        frame: GamepadFrame,
    ) -> bool {
        let now = Instant::now();
        crate::api_xbox::streaming::control::input::note_gamepad_activity(&frame);
        if !self.input_channel_ready {
            if !self.warned_input_not_ready {
                self.warned_input_not_ready = true;
                eprintln!(
                    "Gamepad frames dropped: input channel not ready (handshake stage: {})",
                    self.handshake_label()
                );
            }
            return false;
        }
        // Event-driven input (Step 7 floor lever): a CHANGED gamepad state
        // sends immediately (4 ms floor); UNCHANGED state keeps the 16 ms
        // fixed cadence as the liveness keepalive (parity with reference
        // clients on idle streams).
        let changed = self.last_gamepad.as_ref().map_or(true, |last| *last != frame);
        if input_throttled(
            self.last_input_sent_at
                .map(|sent_at| now.duration_since(sent_at)),
            changed,
        ) {
            return false;
        }
        self.last_input_sent_at = Some(now);
        let Some(bytes) = self.input_queue.queue_gamepad_frames([frame.clone()], true) else {
            return false;
        };
        self.last_gamepad = Some(frame);
        self.send_input_bytes(peer, &bytes)
    }

    fn send_pointer_event_inner(&mut self, peer: &mut RTCPeerConnection, event: PointerEvent) {
        if !self.input_channel_ready {
            return;
        }
        let Some(bytes) = self.input_queue.queue_pointer_frame(PointerFrame {
            events: vec![event],
        }) else {
            return;
        };
        let _ = self.send_input_bytes(peer, &bytes);
    }

    fn handle_channel_open_inner(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: RTCDataChannelId,
    ) {
        let channel_ids = self.channel_ids;
        let kind = if channel_id == channel_ids.message {
            "message"
        } else if channel_id == channel_ids.input {
            "input"
        } else if channel_id == channel_ids.control {
            "control"
        } else {
            "chat/other"
        };
        eprintln!("xHome data channel open: {kind}");
        if channel_id == channel_ids.message
            && self.handshake_stage == HandshakeStage::WaitingForChannels
            && let Some(mut message_channel) = peer.data_channel(channel_id)
        {
            let _ = message_channel.send_text(channel::message_handshake().to_string());
            self.handshake_stage = HandshakeStage::WaitingForHandshakeAck;
            eprintln!("xHome message handshake sent; waiting for HandshakeAck");
        }
        if channel_id == channel_ids.input
            && let Some(mut input_channel) = peer.data_channel(channel_id)
        {
            let client_metadata = self.input_queue.client_metadata_packet(0);
            let _ = input_channel.send(BytesMut::from(client_metadata.as_slice()));
            self.input_channel_ready = true;
            eprintln!("xHome input channel ready - gamepad streaming enabled");
        }
    }

    fn handle_channel_message_inner(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: RTCDataChannelId,
        data: &[u8],
    ) {
        let channel_ids = self.channel_ids;
        channel::handle_data_channel_message(
            peer,
            &channel_ids,
            &mut self.handshake_stage,
            channel_id,
            data,
            self.video_fps,
        );
        if let Some(size) = channel::parse_server_video_size(&channel_ids, channel_id, data) {
            eprintln!("xCloud reported server video size: {size:?}");
            self.server_video_size = Some(size);
        }
    }

    fn notify_keyframe_requested_inner(&mut self, peer: &mut RTCPeerConnection) {
        if let Some(mut control_channel) = peer.data_channel(self.channel_ids.control) {
            let _ = control_channel.send_text(channel::video_keyframe_requested(true).to_string());
        }
    }

    /// Ask the console to reconfigure its video pipeline by asserting a (slightly
    /// alternating) target resolution. A real parameter change forces an encoder
    /// rebuild, which drains its internal queue — the only known lever that resets
    /// accumulated console-side backlog mid-session.
    fn notify_stream_flush_inner(&mut self, peer: &mut RTCPeerConnection) {
        let (width, height) = if self.flush_size_toggle {
            (STREAM_WIDTH, STREAM_HEIGHT)
        } else {
            (STREAM_WIDTH - 16, STREAM_HEIGHT - 8)
        };
        self.flush_size_toggle = !self.flush_size_toggle;
        if let Some(mut control_channel) = peer.data_channel(self.channel_ids.control) {
            let _ =
                control_channel.send_text(channel::dimensions_changed_message(width, height).to_string());
        }
        eprintln!("Requested stream flush via dimensionschanged {width}x{height}");
    }

    fn send_input_bytes(&self, peer: &mut RTCPeerConnection, bytes: &[u8]) -> bool {
        if let Some(mut input_channel) = peer.data_channel(self.channel_ids.input) {
            input_channel.send(BytesMut::from(bytes)).is_ok()
        } else {
            false
        }
    }
}

impl RtcSessionBackend for XboxRtcProtocol {
    fn handle_channel_open(&mut self, peer: &mut RTCPeerConnection, channel_id: RTCDataChannelId) {
        self.handle_channel_open_inner(peer, channel_id);
    }

    fn handle_channel_message(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: RTCDataChannelId,
        data: &[u8],
    ) {
        self.handle_channel_message_inner(peer, channel_id, data);
    }

    fn send_gamepad_frame(&mut self, peer: &mut RTCPeerConnection, frame: GamepadFrame) -> bool {
        self.send_gamepad_frame_inner(peer, frame)
    }

    fn send_pointer_event(&mut self, peer: &mut RTCPeerConnection, event: PointerEvent) {
        self.send_pointer_event_inner(peer, event);
    }

    fn notify_keyframe_requested(&mut self, peer: &mut RTCPeerConnection) {
        self.notify_keyframe_requested_inner(peer);
    }

    fn notify_stream_flush(&mut self, peer: &mut RTCPeerConnection) {
        self.notify_stream_flush_inner(peer);
    }

    fn server_video_size(&self) -> Option<(u32, u32)> {
        self.server_video_size
    }

    fn debug_state(&self) -> String {
        format!(
            "hs:{} in:{}",
            self.handshake_label(),
            if self.input_channel_ready { "y" } else { "n" }
        )
    }
}

#[cfg(test)]
mod input_throttle_tests {
    use super::*;

    #[test]
    fn unchanged_state_keeps_16ms_keepalive() {
        assert!(!input_throttled(None, false), "never sent ⇒ send");
        assert!(!input_throttled(Some(Duration::from_millis(16)), false));
        assert!(input_throttled(Some(Duration::from_millis(15)), false));
    }

    #[test]
    fn changed_state_sends_immediately_with_4ms_floor() {
        assert!(!input_throttled(Some(Duration::from_millis(15)), true), "changed ⇒ bypass cadence");
        assert!(!input_throttled(Some(Duration::from_millis(4)), true));
        assert!(input_throttled(Some(Duration::from_millis(3)), true), "4 ms floor still applies");
    }
}
