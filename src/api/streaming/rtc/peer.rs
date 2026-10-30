use anyhow::{Context, Result};
use rtc::data_channel::{RTCDataChannelId, RTCDataChannelInit};
use rtc::interceptor::{Interceptor, Registry};
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::media_engine::MediaEngine;
use rtc::peer_connection::configuration::setting_engine::SettingEngine;
use rtc::peer_connection::transport::RTCIceServer;
use rtc::peer_connection::{RTCPeerConnection, RTCPeerConnectionBuilder};
use rtc_interceptor::TwccReceiverBuilder;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use std::time::Duration;

/// The interceptor chain fulfilling the minimal RTCP contract (periodic sender/receiver
/// reports only). The concrete chain type is crate-private upstream, so downstream
/// code refers to this alias everywhere instead of bare `RTCPeerConnection`.
pub(crate) type FeedbackChain = impl Interceptor;
pub(crate) type FeedbackPeer = RTCPeerConnection<FeedbackChain>;

#[define_opaque(FeedbackChain)]
pub(crate) fn feedback_registry() -> Result<Registry<FeedbackChain>> {
    // 2c (2026-10-30): bind the TWCC feedback generator — TWCC ALONE, no
    // NACK/PLI/RR responders, behind commit-isolation (revert this binding
    // to disable; the opaque chain type cannot be gated by a const if/else).
    // A2 experiment: the throttle suspect (2500-6000 kbps clamp, 72-328
    // floor crashes). Proof-of-send = twcc_sent counted at the socket
    // (transport flush); experiment's on/off reading is that CSV column.
    // History: this interceptor previously "never fired tw:0 despite
    // negotiated extmap" — misdiagnosed: NOTHING was negotiated pre-2a
    // (zero a=extmap lines). 690f0c7 negotiates twcc (answer id 1) and the
    // 144041 probe session proved inbound annotation live (rtp_ext_any =
    // 135,980/249s). The unsourced "~10x lag" folklore covered the bundled
    // NACK/PLI responder set, not isolated TWCC-send. Lag watch: q_us /
    // stream_slip + user feel; regression => git revert.
    Ok(Registry::new()
        .with(TwccReceiverBuilder::new().with_interval(Duration::from_millis(100)).build()))
/* legacy note kept below for archaeology:
    // Fully silent registry. VERIFIED 2026-10-30 (chrome-dump reanalysis +
    // device last-offer.sdp ground truth) — corrected history:
    // - The "manual TWCC" path NEVER sent a packet: take_twcc_report() ->
    //   None with no TwccReport producer in the tree, twcc_sent=0 in every
    //   device CSV. The old claim "only manual TWCC + REMB flow" was already
    //   stale when written (RECOVERY-PLAN:9 documents the None itself).
    // - "TwccReceiverInterceptor never fired — tw:0 despite negotiated
    //   extmap" was a WRONG diagnosis: our offer contains zero a=extmap
    //   lines (verified last-offer.sdp), extract_extmap_id returns None and
    //   unwrap_or(0) masked it; rtc skips id 0 by design. Nothing was ever
    //   negotiated.
    // - "the console never annotates toward us" (RECOVERY-PLAN:9) is
    //   REFUTED by chrome-dumps: the same console annotated packets densely
    //   enough for Chrome to emit 506-1290 TWCC feedback/session (Chrome's
    //   diet: TWCC+RR, zero REMB).
    // - The "~10x lag" folklore (RECOVERY-PLAN:10, arch 16.2 row5) cites
    //   THIS comment and only this comment — circular, no measurement
    //   exists — and it bundled NACK/PLI *responders* with TWCC.
    // Plan history: 2a/2b landed (690f0c7, 64cd6ee); 2c is the binding above.
    */
}

pub(crate) struct RtcDataChannelConfig {
    pub label: &'static str,
    pub protocol: &'static str,
    pub ordered: bool,
    pub max_packet_life_time: Option<u16>,
    pub max_retransmits: Option<u16>,
}

/// Builds the provider-neutral WebRTC peer. Providers supply only their negotiated codecs,
/// ICE servers and data-channel descriptions.
pub(crate) fn create(
    media_engine: MediaEngine,
    ice_servers: Vec<RTCIceServer>,
    channels: &[RtcDataChannelConfig],
) -> Result<(FeedbackPeer, Vec<RTCDataChannelId>)> {
    // No automatic RTCP (see feedback_registry). Corrections 2026-10-30:
    // - "pacing guidance comes from b=AS/TIAS" is DISPUTED inside our own
    //   corpus: arch 16.3.6 states the console ignores b=AS/TIAS (we inject
    //   b=AS:5000 today; effect undocumented either way).
    // - "full default set made lag grow ~10x" originates in this comment
    //   (RECOVERY-PLAN:222 quotes peer.rs; arch 16.2 quotes RECOVERY-PLAN) —
    //   circular and unsourced; it covered NACK/PLI responders + TWCC, not
    //   TWCC-send-alone, which has never been tested.
    // - "RR/SR feed the console misleading jitter": same unsourced lineage.
    // Verified live behaviour: CONSTANT 15 Mbps REMB @500 ms is the only
    // feedback we send; Chrome, by contrast, sends zero REMB ever.
    let registry = feedback_registry()?;

    let configuration = RTCConfigurationBuilder::new()
        .with_ice_servers(ice_servers)
        .build();
    let mut setting_engine = SettingEngine::default();
    setting_engine.set_ice_connection_attempts(Some(Duration::from_millis(200)), Some(75));

    let mut peer = RTCPeerConnectionBuilder::new()
        .with_configuration(configuration)
        .with_setting_engine(setting_engine)
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build()
        .context("failed to create rtc peer connection")?;

    let mut channel_ids = Vec::with_capacity(channels.len());
    for channel in channels {
        let init = RTCDataChannelInit {
            ordered: channel.ordered,
            max_packet_life_time: channel.max_packet_life_time,
            max_retransmits: channel.max_retransmits,
            protocol: channel.protocol.to_owned(),
            ..Default::default()
        };
        let data_channel = peer
            .create_data_channel(channel.label, Some(init))
            .with_context(|| format!("failed to create {} data channel", channel.label))?;
        channel_ids.push(data_channel.id());
    }

    peer.add_transceiver_from_kind(RtpCodecKind::Video, None)
        .context("failed to add video transceiver")?;
    peer.add_transceiver_from_kind(RtpCodecKind::Audio, None)
        .context("failed to add audio transceiver")?;

    Ok((peer, channel_ids))
}
