//! TWCC vendor module - adapted from rtc-interceptor v0.20.0-rc.2.
//! MIT OR Apache-2.0 ORIGINALLY; adapted for green-vita: import paths and visibility only; logic unchanged.
//!
//! 2c (2026-10-30): wired — fed from VideoRtp::receive(), drained by
//! media::send_twcc() every TWCC_INTERVAL; dead-code allow removed.

pub(crate) mod arrival_time_map;
pub(crate) mod recorder;