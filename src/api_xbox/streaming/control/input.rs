//! Wire format for xCloud's "input" data channel: gamepad + pointer report packets.

use crate::streaming::input::{GamepadFrame, PointerEvent};
use std::time::Duration;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ReportType {
    None = 0,
    Metadata = 1,
    Gamepad = 2,
    Pointer = 4,
    ClientMetadata = 8,
    ServerMetadata = 16,
}

/// Per-rendered-frame render-timing report (reference `render/video.ts`): the
/// console keys these by RTP timestamp to learn real client glass-to-glass
/// latency and pace its pipeline. This is xHome's latency-feedback loop — RTCP
/// is inert toward it (proven experimentally).
#[derive(Debug, Clone, Copy)]
pub struct MetadataFrame {
    pub server_data_key: u32,
    pub first_frame_packet_arrival_ms: u32,
    pub frame_submitted_ms: u32,
    pub frame_decoded_ms: u32,
    pub frame_rendered_ms: u32,
}

struct QueuedVideoMetadata {
    server_data_key: u32,
    arrival: Instant,
}

/// Frames completed by the video path await their render-timing report. The
/// input packetizer drains this queue and piggybacks reports onto regular input
/// traffic exactly like the reference client's InputQueue.
static METADATA_QUEUE: Mutex<VecDeque<QueuedVideoMetadata>> = Mutex::new(VecDeque::new());

/// Timestamp of the most recent gamepad frame carrying real user input (any
/// button, stick or trigger engaged). The auto-reconnect defers session recycles
/// until controls have been quiet, so cuts land between actions.
static LAST_ACTIVE_INPUT: Mutex<Option<Instant>> = Mutex::new(None);

pub(crate) fn note_gamepad_activity(frame: &GamepadFrame) {
    let active = frame.nexus > 0.0
        || frame.menu > 0.0
        || frame.view > 0.0
        || frame.a > 0.0
        || frame.b > 0.0
        || frame.x > 0.0
        || frame.y > 0.0
        || frame.dpad_up > 0.0
        || frame.dpad_down > 0.0
        || frame.dpad_left > 0.0
        || frame.dpad_right > 0.0
        || frame.left_shoulder > 0.0
        || frame.right_shoulder > 0.0
        || frame.left_thumb > 0.0
        || frame.right_thumb > 0.0
        || frame.left_thumb_x_axis.abs() > f32::EPSILON
        || frame.left_thumb_y_axis.abs() > f32::EPSILON
        || frame.right_thumb_x_axis.abs() > f32::EPSILON
        || frame.right_thumb_y_axis.abs() > f32::EPSILON
        || frame.left_trigger > 0.0
        || frame.right_trigger > 0.0;
    if !active {
        return;
    }
    if let Ok(mut slot) = LAST_ACTIVE_INPUT.lock() {
        *slot = Some(Instant::now());
    }
}

pub(crate) fn input_quiet_for(min_quiet: Duration) -> bool {
    match LAST_ACTIVE_INPUT.lock() {
        Ok(slot) => slot.is_none_or(|at| at.elapsed() >= min_quiet),
        Err(_) => true,
    }
}

static SESSION_START: OnceLock<Instant> = OnceLock::new();

pub(crate) const SEND_METADATA: bool = true;

/// Monotonic session-uptime milliseconds — the wire-format equivalent of the
/// reference client's performance.now(). Our old header wrote per-packet elapsed
/// time (~0 for every packet); the console needs a monotonic base.
pub(crate) fn uptime_ms() -> f64 {
    let start = SESSION_START.get_or_init(Instant::now);
    start.elapsed().as_secs_f64() * 1000.0
}

fn uptime_ms_u32_since(at: Instant) -> u32 {
    let start = SESSION_START.get_or_init(Instant::now);
    at.checked_duration_since(*start)
        .map(|duration| duration.as_millis() as u32)
        .unwrap_or(0)
}

/// Called by the video assembly path for every access unit accepted by the decoder.
pub(crate) fn queue_video_metadata(server_data_key: u32, arrival: Instant) {
    let mut queue = METADATA_QUEUE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if queue.len() >= 240 {
        queue.pop_front();
    }
    queue.push_back(QueuedVideoMetadata { server_data_key, arrival });
}

fn drain_video_metadata() -> Vec<MetadataFrame> {
    let mut queue = METADATA_QUEUE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    queue
        .drain(..)
        .map(|queued| {
            let arrival_ms = uptime_ms_u32_since(queued.arrival);
            MetadataFrame {
                server_data_key: queued.server_data_key,
                first_frame_packet_arrival_ms: arrival_ms,
                // Minimal values test best (verified experimentally).
                frame_submitted_ms: arrival_ms,
                frame_decoded_ms: uptime_ms_u32_since(Instant::now()),
                frame_rendered_ms: uptime_ms_u32_since(Instant::now()),
            }
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct PointerFrame {
    pub events: Vec<PointerEvent>,
}

#[derive(Debug, Clone)]
pub struct InputPacket {
    report_type: u16,
    total_size: usize,
    sequence: u32,
    metadata_frames: Vec<MetadataFrame>,
    gamepad_frames: Vec<GamepadFrame>,
    pointer_frames: Vec<PointerFrame>,
    max_touchpoints: u8,
}

impl InputPacket {
    pub fn new(sequence: u32) -> Self {
        Self {
            report_type: ReportType::None as u16,
            total_size: 14,
            sequence,
            metadata_frames: Vec::new(),
            gamepad_frames: Vec::new(),
            pointer_frames: Vec::new(),
            max_touchpoints: 0,
        }
    }

    pub fn client_metadata(sequence: u32, max_touchpoints: u8) -> Self {
        let mut packet = Self::new(sequence);
        packet.report_type = ReportType::ClientMetadata as u16;
        packet.total_size = 15;
        packet.max_touchpoints = max_touchpoints;
        packet
    }

    pub fn set_data(
        &mut self,
        metadata: Vec<MetadataFrame>,
        gamepads: Vec<GamepadFrame>,
        pointers: Vec<PointerFrame>,
    ) {
        let mut size = 14;
        if !metadata.is_empty() {
            self.report_type |= ReportType::Metadata as u16;
            size += 1 + 28 * metadata.len();
        }
        if !gamepads.is_empty() {
            self.report_type |= ReportType::Gamepad as u16;
            size += 1 + 23 * gamepads.len();
        }
        if !pointers.is_empty() {
            self.report_type |= ReportType::Pointer as u16;
            size += 1 + pointers
                .iter()
                .map(|frame| 1 + frame.events.len() * 20)
                .sum::<usize>();
        }

        self.total_size = size;
        self.metadata_frames = metadata;
        self.gamepad_frames = gamepads;
        self.pointer_frames = pointers;
    }

    pub fn to_bytes(&mut self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.total_size);
        push_le(&mut bytes, self.report_type.to_le_bytes());
        push_le(&mut bytes, self.sequence.to_le_bytes());
        push_le(&mut bytes, uptime_ms().to_le_bytes());

        if !self.metadata_frames.is_empty() {
            self.write_metadata(&mut bytes);
        }
        if !self.gamepad_frames.is_empty() {
            self.write_gamepads(&mut bytes);
        }
        if !self.pointer_frames.is_empty() {
            self.write_pointers(&mut bytes);
        }
        if self.report_type == ReportType::ClientMetadata as u16 {
            bytes.push(self.max_touchpoints);
        }

        debug_assert_eq!(bytes.len(), self.total_size);
        bytes
    }

    fn write_metadata(&self, bytes: &mut Vec<u8>) {
        bytes.push(self.metadata_frames.len() as u8);
        let now_u32 = uptime_ms_u32_since(SESSION_START.get().copied().unwrap_or_else(Instant::now));
        for frame in &self.metadata_frames {
            crate::streaming::video::metrics::METRICS
                .video_metadata_sent
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            push_le(bytes, frame.server_data_key.to_le_bytes());
            push_le(bytes, frame.first_frame_packet_arrival_ms.to_le_bytes());
            push_le(bytes, frame.frame_submitted_ms.to_le_bytes());
            push_le(bytes, frame.frame_decoded_ms.to_le_bytes()); // decoded
            push_le(bytes, frame.frame_rendered_ms.to_le_bytes()); // rendered
            push_le(bytes, now_u32.to_le_bytes()); // framePacketTime
            push_le(bytes, now_u32.to_le_bytes()); // frameDateNow
        }
    }

    fn write_gamepads(&self, bytes: &mut Vec<u8>) {
        bytes.push(self.gamepad_frames.len() as u8);
        for input in &self.gamepad_frames {
            bytes.push(input.gamepad_index);
            let mut mask = 0u16;
            let buttons = [
                (input.nexus, 2),
                (input.menu, 4),
                (input.view, 8),
                (input.a, 16),
                (input.b, 32),
                (input.x, 64),
                (input.y, 128),
                (input.dpad_up, 256),
                (input.dpad_down, 512),
                (input.dpad_left, 1024),
                (input.dpad_right, 2048),
                (input.left_shoulder, 4096),
                (input.right_shoulder, 8192),
                (input.left_thumb, 16384),
                (input.right_thumb, 32768),
            ];
            for (value, bit) in buttons {
                if value > 0.0 {
                    mask |= bit;
                }
            }
            push_le(bytes, mask.to_le_bytes());
            push_le(bytes, normalize_axis(input.left_thumb_x_axis).to_le_bytes());
            push_le(
                bytes,
                normalize_axis(-input.left_thumb_y_axis).to_le_bytes(),
            );
            push_le(
                bytes,
                normalize_axis(input.right_thumb_x_axis).to_le_bytes(),
            );
            push_le(
                bytes,
                normalize_axis(-input.right_thumb_y_axis).to_le_bytes(),
            );
            push_le(bytes, normalize_trigger(input.left_trigger).to_le_bytes());
            push_le(bytes, normalize_trigger(input.right_trigger).to_le_bytes());
            push_le(bytes, 1u32.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_be_bytes());
        }
    }

    fn write_pointers(&self, bytes: &mut Vec<u8>) {
        bytes.push(1);
        if let Some(frame) = self.pointer_frames.first() {
            bytes.push(frame.events.len() as u8);
            for event in &frame.events {
                push_le(bytes, event.contact_major.to_le_bytes());
                push_le(bytes, event.contact_minor.to_le_bytes());
                bytes.push(event.pressure);
                push_le(bytes, event.twist.to_le_bytes());
                push_le(bytes, 0u32.to_le_bytes());
                push_le(bytes, event.x.to_le_bytes());
                push_le(bytes, event.y.to_le_bytes());
                bytes.push(event.event_type);
            }
        }
    }
}

fn normalize_trigger(value: f32) -> u16 {
    if value < 0.0 {
        0
    } else {
        (65535.0 * value).clamp(0.0, 65535.0) as u16
    }
}

fn normalize_axis(value: f32) -> i16 {
    let max = 32767.0;
    (value * max).clamp(-max, max) as i16
}

fn push_le<const N: usize>(bytes: &mut Vec<u8>, raw: [u8; N]) {
    bytes.extend_from_slice(&raw);
}

/// Batches queued frames into [`InputPacket`]s, one wire packet per send.
#[derive(Debug, Default)]
pub struct InputQueue {
    sequence: u32,
    gamepads: Vec<GamepadFrame>,
    pointers: Vec<PointerFrame>,
}

impl InputQueue {
    pub fn queue_gamepad_frames(
        &mut self,
        frames: impl IntoIterator<Item = GamepadFrame>,
        force_send: bool,
    ) -> Option<Vec<u8>> {
        self.gamepads.extend(frames);
        self.check_queue_and_packet(force_send)
    }

    pub fn queue_pointer_frame(&mut self, frame: PointerFrame) -> Option<Vec<u8>> {
        if let Some(existing) = self.pointers.first_mut() {
            existing.events.extend(frame.events);
        } else {
            self.pointers.push(frame);
        }
        self.check_queue_and_packet(true)
    }

    pub fn client_metadata_packet(&mut self, max_touchpoints: u8) -> Vec<u8> {
        InputPacket::client_metadata(self.next_sequence(), max_touchpoints).to_bytes()
    }

    fn check_queue_and_packet(&mut self, force_send: bool) -> Option<Vec<u8>> {
        // Reference flush rules: >5 pending metadata frames force a packet,
        // smaller batches ride along with any other report type, and per-rAF
        // batching keeps ~18-30 ms between packets. Gate metadata-only flushes on
        // the oldest queued frame aging ≥30 ms so a 60 fps queue can't emit one
        // packet per poll tick.
        let metadata_due = METADATA_QUEUE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .front()
            .is_some_and(|queued| queued.arrival.elapsed() >= Duration::from_millis(30));
        let should_send = force_send || !self.gamepads.is_empty() || !self.pointers.is_empty() || metadata_due;
        should_send.then(|| self.drain_packet())
    }

    fn drain_packet(&mut self) -> Vec<u8> {
        let mut packet = InputPacket::new(self.next_sequence());
        packet.set_data(
            drain_video_metadata(),
            std::mem::take(&mut self.gamepads),
            std::mem::take(&mut self.pointers),
        );
        packet.to_bytes()
    }

    fn next_sequence(&mut self) -> u32 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
}
