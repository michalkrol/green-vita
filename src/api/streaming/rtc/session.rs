use crate::api::streaming::rtc::media::{AudioReceiver, VideoReceiver, set_negotiated_rtx_map};
use crate::api::streaming::rtc::transport::RtcTransport;
use crate::streaming::input::{GamepadFrame, PointerEvent};
use crate::streaming::video::{DecoderConfig, DirectVideoOutput};
use crate::streaming::video::metrics::METRICS;
use anyhow::{Context, Result};
use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use rtc::peer_connection::event::{RTCDataChannelEvent, RTCPeerConnectionEvent, RTCTrackEvent};
use rtc::peer_connection::message::RTCMessage;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::state::RTCPeerConnectionState;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::statistics::StatsSelector;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use rtc::sansio::Protocol;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const KEYFRAME_REQUEST_COOLDOWN: Duration = Duration::from_millis(150);
/// Sustained keyframe demand means recovery is failing; spamming PLI/IDR requests
/// every 300 ms overloads the console encoder and deepens its backlog.
const KEYFRAME_STORM_WINDOW: Duration = Duration::from_secs(4);
const KEYFRAME_STORM_THRESHOLD: usize = 8;
const KEYFRAME_STORM_COOLDOWN: Duration = Duration::from_millis(500);
/// When measured glass lag stays above this for LAG_FLUSH_SUSTAIN, assume the
/// console-side queue is backlogged and force an encoder reconfigure to flush it.
/// Lowered from 150ms to 75ms for faster backlog detection.
const LAG_FLUSH_THRESHOLD_US: u64 = 75_000;
const LAG_FLUSH_SUSTAIN: Duration = Duration::from_millis(300);
const LAG_FLUSH_MIN_INTERVAL: Duration = Duration::from_secs(3);
/// Independent trigger: each dropped access unit corresponds to a console-side
/// shed/skip event whose pacer slip never recovers; flush after this many new
/// drops even if the (re-anchoring) lag reading hasn't caught up.
/// Lowered from 25 to 10 for earlier detection.
const DROP_FLUSH_THRESHOLD: u64 = 10;
const INITIAL_VIDEO_GRACE: Duration = Duration::from_millis(500);
const INITIAL_VIDEO_KEYFRAME_INTERVAL: Duration = Duration::from_millis(500);
/// When true, no keyframe requests are sent (initial watchdog, recovery PLI).
/// Expect prolonged decode artifacts; resyncs wait for the console's own GOP IDR.
const SUPPRESS_KEYFRAME_REQUESTS: bool = false;

/// Gates the lag half of the backlog flush trigger.  The metric is being
/// validated observe-only first: with this false the HUD shows live lag/lagMax
/// while flush behaviour is unchanged (the drop-count trigger still applies).
const LAG_FLUSH_ENABLED: bool = false;

/// Gates the backlog flush trigger ENTIRELY.  Disabled while we test whether the
/// flush feedback loop is responsible for the artefacts and the steady latency
/// drift observed at a high bitrate cap: the drop-count trigger fired 29 times in
/// 17 minutes (one flush every ~20 s, sometimes only 6 s apart), and every flush
/// asks the console to rebuild its encoder via `dimensionschanged`.  With this
/// false both the drop-count and the lag triggers no-op; drops and lag are still
/// counted and logged.  Supersedes LAG_FLUSH_ENABLED while disabled.
const FLUSH_TRIGGER_ENABLED: bool = false;

/// Prints the per-second HUD/status line to stderr.  Disabled because it runs on
/// the latency-critical RTC pump thread and stderr is blocking I/O on the Vita;
/// the same data is available in the metrics CSV.
const LOG_STATUS_TO_STDERR: bool = false;

pub(crate) struct RtcSessionConfig {
    pub stun_server: &'static str,
    pub route_probe: &'static str,
    pub audio_sample_rate: u32,
    pub audio_payload_type: u8,
    pub video_fps: u32,
    pub decoder: DecoderConfig,
    pub remb_interval_ms: u32,
    pub twcc_delay_injection: bool,
    pub twcc_delay_ramp_max: u8,
    pub periodic_keyframe_enabled: bool,
    pub remb_base_cap_bps: u32,
}

/// Provider-specific hooks invoked by the reusable RTC session.
pub(crate) trait RtcSessionBackend {
    fn handle_channel_open(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: rtc::data_channel::RTCDataChannelId,
    );
    fn handle_channel_message(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: rtc::data_channel::RTCDataChannelId,
        data: &[u8],
    );
    fn send_gamepad_frame(&mut self, peer: &mut RTCPeerConnection, frame: GamepadFrame) -> bool;
    fn send_pointer_event(&mut self, peer: &mut RTCPeerConnection, event: PointerEvent);
    fn notify_keyframe_requested(&mut self, peer: &mut RTCPeerConnection);
    /// Provider hook to force a console-side stream/encoder reconfiguration.
    fn notify_stream_flush(&mut self, peer: &mut RTCPeerConnection);
    fn server_video_size(&self) -> Option<(u32, u32)>;
    /// Optional provider-specific HUD suffix (handshake/input state etc.).
    fn debug_state(&self) -> String {
        String::new()
    }
}

pub(crate) struct RtcSession<B: RtcSessionBackend> {
    pub(crate) peer: RTCPeerConnection,
    pub(crate) transport: RtcTransport,
    pub(crate) backend: B,
    pub connection_state: RTCPeerConnectionState,
    pub(crate) video: VideoReceiver,
    pub(crate) audio: AudioReceiver,
    last_keyframe_request: Option<Instant>,
    recent_keyframe_requests: Vec<Instant>,
    lag_high_since: Option<Instant>,
    last_lag_flush: Option<Instant>,
    drops_at_last_flush: u64,
    initial_video_watchdog_started_at: Option<Instant>,
    last_initial_video_keyframe_request: Option<Instant>,
    /// Periodic keyframe PLI interval (200ms) — experimental, may force encoder catchup.
    periodic_keyframe_interval: Option<Duration>,
    last_periodic_keyframe: Option<Instant>,
    /// Counter for RTP packets whose track_id matches neither video nor audio.
    /// Logged for first few occurrences to diagnose RTX SSRC delivery.
    unknown_track_packets: u32,
    /// Last time we sampled crate RTX stats via get_stats().
    last_stats_sample: Option<Instant>,
    pub status: String,
}

impl<B: RtcSessionBackend> RtcSession<B> {
    pub async fn new(
        mut peer: RTCPeerConnection,
        backend: B,
        config: RtcSessionConfig,
        direct_output: Arc<DirectVideoOutput>,
    ) -> Result<Self> {
        let transport =
            RtcTransport::bind(&mut peer, config.stun_server, config.route_probe).await?;
        let video = VideoReceiver::new(
            config.decoder,
            direct_output,
            config.video_fps,
            config.remb_interval_ms,
            config.twcc_delay_injection,
            config.twcc_delay_ramp_max,
            config.remb_base_cap_bps,
        )?;

        let periodic_keyframe_interval = if config.periodic_keyframe_enabled {
            Some(Duration::from_millis(200))
        } else {
            None
        };

        Ok(Self {
            peer,
            transport,
            backend,
            connection_state: RTCPeerConnectionState::New,
            video,
            audio: AudioReceiver::new(config.audio_sample_rate, config.audio_payload_type),
            last_keyframe_request: None,
            recent_keyframe_requests: Vec::new(),
            lag_high_since: None,
            last_lag_flush: None,
            drops_at_last_flush: 0,
            initial_video_watchdog_started_at: None,
            last_initial_video_keyframe_request: None,
            periodic_keyframe_interval,
            last_periodic_keyframe: None,
            unknown_track_packets: 0,
            last_stats_sample: None,
            status: "Negotiating WebRTC connection".to_owned(),
        })
    }

    pub fn create_offer(&mut self) -> Result<RTCSessionDescription> {
        let offer = self
            .peer
            .create_offer(None)
            .context("failed to create rtc offer")?;
        self.peer
            .set_local_description(offer.clone())
            .context("failed to set local rtc description")?;
        Ok(offer)
    }

    pub fn set_remote_answer(&mut self, sdp: impl Into<String>) -> Result<()> {
        let answer_str: String = sdp.into();
        // Extract negotiated RTX payload types from the answer SDP before setting it.
        // The console may renumber payload types, so the answer is the source of truth.
        let rtx_map = crate::api_xbox::streaming::rtc::sdp::extract_negotiated_rtx_mapping(&answer_str);
        // Log unconditionally with diagnostics to catch parser misses.
        let sdp_lines = answer_str.lines().count();
        let rtx_rtpmap_count = answer_str.lines().filter(|l| l.trim_start().starts_with("a=rtpmap:") && l.to_ascii_lowercase().contains("rtx/")).count();
        let apt_fmtp_count = answer_str.lines().filter(|l| l.trim_start().starts_with("a=fmtp:") && l.contains("apt=")).count();
        eprintln!(
            "SDP answer RTX parse: lines_scanned={} rtx_rtpmap_lines={} apt_fmtp_lines={} map={:?}",
            sdp_lines, rtx_rtpmap_count, apt_fmtp_count, rtx_map
        );
        set_negotiated_rtx_map(rtx_map);
        let answer = RTCSessionDescription::answer(answer_str);
        self.peer
            .set_remote_description(answer?)
            .context("failed to set remote rtc answer")
    }

    pub fn close(&mut self) -> Result<()> {
        self.peer
            .close()
            .context("failed to close rtc peer connection")
    }

    pub fn add_remote_candidate(&mut self, candidate: RTCIceCandidateInit) -> Result<()> {
        self.peer
            .add_remote_candidate(candidate)
            .context("failed to add remote ICE candidate")
    }

    pub async fn pump(&mut self) -> Result<Vec<RTCIceCandidateInit>> {
        self.transport.flush(&mut self.peer).await;
        self.transport.receive(&mut self.peer);
        let gathered_candidates = self.handle_peer_events();
        let mut keyframe_requested = self.handle_peer_messages();
        self.video.drain_decoder(&mut keyframe_requested);

        let now = Instant::now();
        // rtc-rs is sans-I/O, so its expired internal timer must be advanced by our pump.
        if let Some(deadline) = self.peer.poll_timeout()
            && now >= deadline
        {
            let _ = self.peer.handle_timeout(now);
        }
        if !SUPPRESS_KEYFRAME_REQUESTS && self.initial_video_keyframe_due(now) {
            eprintln!("No initial video RTP after WebRTC connected; requesting a keyframe");
            keyframe_requested = true;
        }
        self.request_keyframe(keyframe_requested, now);
        // Periodic keyframe (PLI) — experimental: may force encoder catchup
        // when lag accumulates by resetting the GOP chain.
        if let Some(interval) = self.periodic_keyframe_interval
            && self.connection_state == RTCPeerConnectionState::Connected
        {
            let due = self
                .last_periodic_keyframe
                .is_none_or(|last| now.duration_since(last) >= interval);
            if due {
                self.video.request_keyframe(&mut self.peer);
                self.last_periodic_keyframe = Some(now);
            }
        }
        // REMB+TWCC feedback: keeps the console's bandwidth estimate fresh.
        self.video.send_remb(&mut self.peer, now);
        self.video.send_twcc(&mut self.peer, now);
        // NACK feedback: request retransmission of missing RTP packets.
        self.video.send_nacks(&mut self.peer, now);
        self.maybe_flush_backlog(now);
        if let Some(status) = self.video.status(now) {
            let debug_state = self.backend.debug_state();
            self.status = if let Some((width, height)) = self.backend.server_video_size() {
                format!("srv:{width}x{height} {status} {debug_state}")
            } else {
                format!("srv:? {status} {debug_state}")
            };
            if LOG_STATUS_TO_STDERR {
                eprintln!("{}", self.status);
            }
        }

        // Sample crate RTX stats once per second (Task 1: prove console retransmits).
        // The stats accumulator counts RTX packets BEFORE the endpoint drops them.
        if self.last_stats_sample.is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1)) {
            self.last_stats_sample = Some(now);
            let report = self.peer.get_stats(now, StatsSelector::None);
            let mut total_rtx_pkts = 0u64;
            let mut total_rtx_bytes = 0u64;
            for stream in report.inbound_rtp_streams() {
                // Only count video streams
                if stream.received_rtp_stream_stats.rtp_stream_stats.kind == RtpCodecKind::Video {
                    total_rtx_pkts = total_rtx_pkts.saturating_add(stream.retransmitted_packets_received);
                    total_rtx_bytes = total_rtx_bytes.saturating_add(stream.retransmitted_bytes_received);
                }
            }
            if total_rtx_pkts > 0 || total_rtx_bytes > 0 {
                METRICS.rtx_crate_received.fetch_add(total_rtx_pkts, Ordering::Relaxed);
                METRICS.rtx_crate_bytes.fetch_add(total_rtx_bytes, Ordering::Relaxed);
            }
        }

        Ok(gathered_candidates)
    }

    fn initial_video_keyframe_due(&mut self, now: Instant) -> bool {
        if self.video.received_packet {
            self.initial_video_watchdog_started_at = None;
            self.last_initial_video_keyframe_request = None;
            return false;
        }
        if self.connection_state != RTCPeerConnectionState::Connected {
            self.initial_video_watchdog_started_at = None;
            self.last_initial_video_keyframe_request = None;
            return false;
        }

        let connected_at = *self.initial_video_watchdog_started_at.get_or_insert(now);
        if now.duration_since(connected_at) < INITIAL_VIDEO_GRACE {
            return false;
        }
        if self
            .last_initial_video_keyframe_request
            .is_some_and(|requested_at| {
                now.duration_since(requested_at) < INITIAL_VIDEO_KEYFRAME_INTERVAL
            })
        {
            return false;
        }

        self.last_initial_video_keyframe_request = Some(now);
        true
    }

    fn handle_peer_events(&mut self) -> Vec<RTCIceCandidateInit> {
        let mut gathered_candidates = Vec::new();
        while let Some(event) = self.peer.poll_event() {
            match event {
                RTCPeerConnectionEvent::OnConnectionStateChangeEvent(state) => {
                    self.connection_state = state;
                    self.status = format!("WebRTC connection: {state:?}");
                }
                RTCPeerConnectionEvent::OnIceCandidateEvent(ice_event) => {
                    if let Ok(candidate) = ice_event.candidate.to_json() {
                        gathered_candidates.push(candidate);
                    }
                }
                RTCPeerConnectionEvent::OnIceCandidateErrorEvent(error) => {
                    eprintln!(
                        "ICE candidate error from {} ({}:{}): {} {}",
                        error.url, error.address, error.port, error.error_code, error.error_text
                    );
                }
                RTCPeerConnectionEvent::OnTrack(RTCTrackEvent::OnOpen(init)) => {
                    let track_kind = self
                        .peer
                        .rtp_receiver(init.receiver_id)
                        .map(|receiver| receiver.track().kind());
                    match track_kind {
                        Some(RtpCodecKind::Video) => {
                            self.video.open(init.track_id, init.receiver_id, init.ssrc);
                            self.status = "Receiving video track".to_owned();
                        }
                        Some(RtpCodecKind::Audio) => {
                            self.audio.open(init.track_id);
                            self.status = "Receiving audio track".to_owned();
                        }
                        _ => {}
                    }
                }
                RTCPeerConnectionEvent::OnDataChannel(RTCDataChannelEvent::OnOpen(channel_id)) => {
                    self.backend.handle_channel_open(&mut self.peer, channel_id);
                }
                _ => {}
            }
        }
        gathered_candidates
    }

    fn handle_peer_messages(&mut self) -> bool {
        let mut keyframe_requested = false;
        while let Some(message) = self.peer.poll_read() {
            match message {
                RTCMessage::RtpPacket(track_id, packet) => {
                    if self.video.handles(&track_id) {
                        self.video.receive(&mut self.peer, packet, &mut keyframe_requested);
                    } else if self.audio.handles(&track_id) {
                        self.audio.receive(packet);
                    } else {
                        // Packet for a track we don't handle (e.g., RTX SSRC).
                        // Count in global metrics for CSV and log first few for diagnostics.
                        crate::streaming::video::metrics::METRICS
                            .unknown_track_packets
                            .fetch_add(1, Ordering::Relaxed);
                        let count = self.unknown_track_packets + 1;
                        self.unknown_track_packets = count;
                        if count <= 10 {
                            eprintln!(
                                "Unknown track packet: track_id={:?} ssrc={} pt={} (count={})",
                                track_id, packet.header.ssrc, packet.header.payload_type, count
                            );
                        }
                    }
                }
                RTCMessage::DataChannelMessage(channel_id, data_message) => {
                    self.backend.handle_channel_message(
                        &mut self.peer,
                        channel_id,
                        &data_message.data,
                    );
                }
                RTCMessage::RtcpPacket(track_id, rtcp_packets) => {
                    if self.video.handles(&track_id) {
                        self.video.handle_rtcp(&rtcp_packets);
                    }
                }
            }
        }
        keyframe_requested
    }

    fn request_keyframe(&mut self, requested: bool, now: Instant) {
        if !requested || SUPPRESS_KEYFRAME_REQUESTS {
            return;
        }
        self.recent_keyframe_requests
            .retain(|at| now.duration_since(*at) < KEYFRAME_STORM_WINDOW);
        let cooldown = if self.recent_keyframe_requests.len() >= KEYFRAME_STORM_THRESHOLD {
            KEYFRAME_STORM_COOLDOWN
        } else {
            KEYFRAME_REQUEST_COOLDOWN
        };
        let cooldown_elapsed = self.last_keyframe_request.is_none_or(|requested_at| {
            now.duration_since(requested_at) >= cooldown
        });
        if !cooldown_elapsed {
            return;
        }

        self.recent_keyframe_requests.push(now);
        self.last_keyframe_request = Some(now);
        self.backend.notify_keyframe_requested(&mut self.peer);
        self.video.request_keyframe(&mut self.peer);
    }

    /// Sustained high measured lag means the console queue is backlogged (client
    /// pipeline is otherwise clean). Force an encoder reconfigure to flush it and
    /// re-anchor the HUD so post-flush drift stays measurable.
    fn maybe_flush_backlog(&mut self, now: Instant) {
        use crate::streaming::video::metrics::METRICS;
        if self.connection_state != RTCPeerConnectionState::Connected {
            return;
        }
        let total_drops = self.video.total_drops();
        let drops_since_flush = total_drops.saturating_sub(self.drops_at_last_flush);
        let lag_us = METRICS.stream_lag_us.load(Ordering::Relaxed);
        let lag_eligible = LAG_FLUSH_ENABLED && lag_us >= LAG_FLUSH_THRESHOLD_US;
        let drop_eligible = drops_since_flush >= DROP_FLUSH_THRESHOLD;
        let eligible = FLUSH_TRIGGER_ENABLED && (lag_eligible || drop_eligible);
        if !eligible {
            // Only an eligible stretch counts toward the sustain window; without
            // this reset a single eligible instant would permanently satisfy
            // LAG_FLUSH_SUSTAIN and flush on the next eligible tick.
            self.lag_high_since = None;
            return;
        }
        let since = *self.lag_high_since.get_or_insert(now);
        if now.duration_since(since) < LAG_FLUSH_SUSTAIN {
            return;
        }
        if self
            .last_lag_flush
            .is_some_and(|at| now.duration_since(at) < LAG_FLUSH_MIN_INTERVAL)
        {
            return;
        }
        self.last_lag_flush = Some(now);
        self.lag_high_since = None;
        self.drops_at_last_flush = total_drops;
        eprintln!(
            "stream flush: lag={lag_us}us drops_since={drops_since_flush} total={total_drops}"
        );
        METRICS.flushes.fetch_add(1, Ordering::Relaxed);
        self.backend.notify_stream_flush(&mut self.peer);
        self.video.reset_lag_anchor();
    }

    }
