/// H.264 access-unit assembly from RTP packets.
///
/// This module implements the `AccessUnitAssembler` which builds access units
/// (AUs) from incoming RTP packets, handling FU-A fragmentation, sequence
/// gap detection, IDR detection, and size-based discarding.
///
/// Packetization matches the real console stream as observed in green-vita:
/// - Single NAL units (non-fragmented) are assembled when a marker bit is seen.
/// - FU-A fragments are stitched together when the full FU-A range is present.
/// - The H3 fix ensures AUs whose first packet is not a verified AU start
///   resolve as `Damaged`, never `Complete` (avoids the bug at rtp.rs where
///   `expected_sequence==None` assembled leading-slice-less frames as Complete).
/// - Sequence gaps use modular-16-bit wrapping.
/// - A 12 ms hold window prevents recently-sent AUs from being discarded too early.
/// - Oversize AUs (>2 MiB) are discarded to prevent decoder overflow.
pub mod nal;
pub mod assembler;
pub mod gop;