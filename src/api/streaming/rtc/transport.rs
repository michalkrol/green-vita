use super::ice;
use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use crate::streaming::video::metrics::METRICS;
use anyhow::{Context, Result};
use bytes::BytesMut;
use rtc::sansio::Protocol;
use rtc::shared::{TaggedBytesMut, TransportContext, TransportProtocol};
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

/// A datagram captured by the dedicated receive thread together with its true
/// wire-arrival instant.
struct CapturedDatagram {
    data: Vec<u8>,
    peer_addr: SocketAddr,
    at: Instant,
    /// µs since RX_CLOCK at capture (true wire arrival) — feeds TWCC stamps.
    at_us: u64,
}

/// UDP/ICE transport shared by WebRTC streaming providers.
pub(crate) struct RtcTransport {
    pub(crate) socket: UdpSocket,
    local_addr: SocketAddr,
    datagrams: Receiver<CapturedDatagram>,
}

impl RtcTransport {
    pub(crate) async fn bind(
        peer: &mut RTCPeerConnection,
        stun_server: &str,
        route_probe: &str,
    ) -> Result<Self> {
        let std_socket = StdUdpSocket::bind("0.0.0.0:0")
            .context("failed to bind UDP socket for WebRTC transport")?;
        std_socket
            .set_nonblocking(true)
            .context("failed to set UDP socket non-blocking")?;
        let socket_addr = std_socket
            .local_addr()
            .context("failed to read local UDP socket address")?;
        let local_addr = ice::add_host_candidate(peer, socket_addr.port(), route_probe)
            .context("failed to add local host ICE candidate")?;

        // Dedicated RX thread: drains the kernel buffer within ~1 ms regardless of
        // render/presentation load on the UI thread (vita-moonlight-relay's fix for
        // "image lags behind while controls responsive"). The cloned fd shares the
        // O_NONBLOCK flag with the async twin on POSIX, so the thread polls with a
        // 1 ms sleep instead of blocking.
        let rx_socket = std_socket
            .try_clone()
            .context("failed to clone UDP socket for receive thread")?;
        let socket = UdpSocket::from_std(std_socket)
            .context("failed to register UDP socket with the async runtime")?;

        match ice::discover_server_reflexive_candidate(&socket, local_addr, stun_server).await {
            Ok(Some(public_addr)) => {
                if let Err(error) =
                    ice::add_srflx_candidate(peer, public_addr, local_addr, stun_server)
                {
                    eprintln!("Failed to add server-reflexive ICE candidate: {error:#}");
                }
            }
            Ok(None) => eprintln!("STUN request produced no usable response"),
            Err(error) => eprintln!("STUN discovery failed: {error:#}"),
        }

        ice::local_candidate(peer, String::new(), None)
            .context("failed to signal end-of-candidates")?;

        let (tx, datagrams) = mpsc::sync_channel::<CapturedDatagram>(2048);
        thread::Builder::new()
            .name("rtc-recv".into())
            .spawn(move || {
                // Pin RX to its own core so render/GUI load on the main thread
                // cannot delay datagram pickup; main thread pins itself to core 0.
                unsafe {
                    let result = vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(
                        vitasdk_sys::SCE_KERNEL_THREAD_ID_SELF as _,
                        0b0010,
                    );
                    if result != 0 {
                        eprintln!("rtc-recv affinity pin failed: {result}");
                    }
                }
                let mut buf = vec![0u8; 2048];
                // Arm the arrival clock BEFORE the first recv so every
                // capture instant is >= the anchor (no duration underflow).
                std::sync::LazyLock::force(&crate::api::streaming::rtc::rtp::RX_CLOCK);
                loop {
                    match rx_socket.recv_from(&mut buf) {
                        Ok((n, peer_addr)) => {
                            // Raw SR census at the true wire layer (RFC 7983:
                            // V=2 + PT=200; SRTCP keeps the 8-byte header in
                            // clear; STUN/DTLS have V!=2 and RTP payload types
                            // never equal 200 under mux rules). Splits "console
                            // never SRs us" from "fork drops inbound RTCP after
                            // this point" — compare vs CSV rtcp_sr_count (the
                            // crate's post-read stats).
                            if n >= 2 && buf[0] >> 6 == 2 && buf[1] == 200 {
                                METRICS.rtcp_sr_raw.fetch_add(1, Ordering::Relaxed);
                                // Parse SR anchor from the first 24 bytes (RFC 3550 SR header):
                                //   0-3: version/padding/count/payload type
                                //   4-7: sender SSRC
                                //   8-15: NTP timestamp (64-bit, big-endian, ms since 1900)
                                //   16-19: RTP timestamp (32-bit, big-endian, 90 kHz clock)
                                if n >= 28 {
                                    // NTP 64-bit is seconds:32 | fraction:32 since 1900;
                                    // convert to ms since 1900 (what the ref's math expects).
                                    let ntp64 = u64::from_be_bytes(buf[8..16].try_into().unwrap());
                                    let sr_anchor_ntp = (ntp64 >> 32) * 1000 + (((ntp64 & 0xFFFF_FFFF) * 1000) >> 32);
                                    let sr_anchor_rtp_ts = u64::from(u32::from_be_bytes(buf[16..20].try_into().unwrap()));
                                    let sr_anchor_local_us = crate::api::streaming::rtc::media::PROCESS_START.elapsed().as_micros() as u64;
                                    // Wall clock at SR arrival: UNIX epoch microseconds.
                                    let sr_anchor_local_wall_us = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_micros() as u64)
                                        .unwrap_or(0);
                                    METRICS.sr_anchor_ntp.store(sr_anchor_ntp, Ordering::Relaxed);
                                    METRICS.sr_anchor_rtp_ts.store(sr_anchor_rtp_ts, Ordering::Relaxed);
                                    METRICS.sr_anchor_local_us.store(sr_anchor_local_us, Ordering::Relaxed);
                                    METRICS.sr_anchor_local_wall_us.store(sr_anchor_local_wall_us, Ordering::Relaxed);
                                    // If the console encrypts SRTCP, the body is ciphertext;
                                    // downstream guards (ntp>0, delta_ts<=900_000) suppress garbage.
                                }
                            }
                            let at = Instant::now();
                            let datagram = CapturedDatagram {
                                data: buf[..n].to_vec(),
                                peer_addr,
                                at,
                                at_us: at
                                    .duration_since(*crate::api::streaming::rtc::rtp::RX_CLOCK)
                                    .as_micros() as u64,
                            };
                            if tx.send(datagram).is_err() {
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => {
                            eprintln!("WebRTC receive thread stopped: {error}");
                            break;
                        }
                    }
                }
            })
            .context("failed to spawn WebRTC receive thread")?;

        Ok(Self {
            socket,
            local_addr,
            datagrams,
        })
    }

    pub(crate) async fn flush(&self, peer: &mut RTCPeerConnection) {
        while let Some(outgoing) = peer.poll_write() {
            // Counts TWCC feedback (RTPFB, PT=205, FMT=15) that actually
            // leaves the socket. VERIFIED 2026-10-30: has counted ZERO — the
            // registry is silent and the manual sender is stubbed; this
            // counter goes live with 2c (flag-gated TWCC-alone send).
            let bytes = &outgoing.message;
            // 2c diagnostic: ANY RTPFB (PT=205) leaving, any FMT — splits
            // "interceptor emits nothing" from "emits but mis-tagged".
            if bytes.len() >= 2 && bytes[1] == 205 {
                crate::streaming::video::metrics::METRICS
                    .rtcp_rtpfb_any
                    .fetch_add(1, Ordering::Relaxed);
            }
            if bytes.len() >= 2 && bytes[1] == 205 && bytes[0] & 0x1F == 15 {
                crate::streaming::video::metrics::METRICS
                    .twcc_sent
                    .fetch_add(1, Ordering::Relaxed);
            }
            // S5b: NACK (RTPFB FMT=1) leaving the socket — socket-truth
            // count (packets, not seqs) for CSV rtcp_nack_sent. Also feeds
            // the Safe-Ceiling REMB ladder via its own accumulator (kept
            // separate from the metrics drain so neither steals counts).
            if bytes.len() >= 2 && bytes[1] == 205 && bytes[0] & 0x1F == 1 {
                crate::streaming::video::metrics::METRICS
                    .rtcp_nack_sent
                    .fetch_add(1, Ordering::Relaxed);
                crate::api::streaming::rtc::media::REMB_NACK_EGRESS
                    .fetch_add(1, Ordering::Relaxed);
            }
            if let Err(error) = self
                .socket
                .send_to(&outgoing.message, outgoing.transport.peer_addr)
                .await
            {
                eprintln!(
                    "Failed to send WebRTC UDP packet to {}: {error}",
                    outgoing.transport.peer_addr
                );
            }
        }
    }

    pub(crate) fn receive(&mut self, peer: &mut RTCPeerConnection) {
        loop {
            match self.datagrams.try_recv() {
                Ok(datagram) => {
                    // TWCC socket-arrival stapling: tag video-RTP datagrams
                    // (our negotiated PTs 102/112 — deterministic offer,
                    // echoed in every archived answer) with their capture
                    // instant. Popped by rtp::take_twcc_arrival at the
                    // recorder site.
                    if datagram.data.len() >= 4
                        && datagram.data[0] >> 6 == 2
                        && matches!(datagram.data[1] & 0x7F, 102 | 112)
                    {
                        let seq = u16::from_be_bytes([datagram.data[2], datagram.data[3]]);
                        crate::api::streaming::rtc::rtp::offer_twcc_arrival(seq, datagram.at_us);
                    }
                    if let Err(error) = peer.handle_read(TaggedBytesMut {
                        now: datagram.at,
                        transport: TransportContext {
                            local_addr: self.local_addr,
                            peer_addr: datagram.peer_addr,
                            ecn: None,
                            transport_protocol: TransportProtocol::UDP,
                        },
                        message: BytesMut::from(datagram.data.as_slice()),
                    }) {
                        eprintln!(
                            "Failed to handle WebRTC UDP packet from {}: {error}",
                            datagram.peer_addr
                        );
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    eprintln!("WebRTC receive channel closed");
                    break;
                }
            }
        }
    }
}
