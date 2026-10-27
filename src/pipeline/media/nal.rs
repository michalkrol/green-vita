/// H.264 NAL unit header parsed from the first byte of a NAL unit.
/// The forbidden_zero bit (bit 0) must be 0; packets with forbidden_zero=1
/// are rejected as malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalHeader {
    pub nal_ref_idc: u8,
    pub nal_unit_type: u8,
}

impl NalHeader {
    /// Parse a NAL header from the first byte of a NAL unit payload.
    /// Returns `None` if the forbidden_zero bit is set (malformed).
    pub fn parse_first_byte(b: u8) -> Option<Self> {
        // forbidden_zero bit must be 0 (bit 0, the MSB)
        if b & 0x80 != 0 {
            return None;
        }
        let nal_ref_idc = (b >> 5) & 0x03;
        let nal_unit_type = b & 0x1F;
        Some(Self {
            nal_ref_idc,
            nal_unit_type,
        })
    }

    /// Returns `true` if this NAL unit is an IDR slice.
    pub fn is_keyframe_nal(nal_type: u8) -> bool {
        nal_type == 5 // IDR
    }
}

/// FU-A (Fragmentation Unit A) header parsed from the first two bytes of an
/// FU-A payload (type 28 in the NAL unit type field).
///
/// The FU-A header format per H.264 §6.2.2.1:
///   - First byte: D (1 bit, forbidden_zero) | R (2 bits, nal_ref_idc) | Type (5 bits, lower 5 bits of nal_unit_type)
///   - Second byte: ST (1 bit, start flag) | ET (1 bit, end flag) | MT (1 bit) | Type (5 bits, upper 5 bits of nal_unit_type)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuHeader {
    pub start: bool,   // FU start bit (ST, MSB of second payload byte)
    pub end: bool,     // FU end bit (ET, second MSB of second payload byte)
    pub nal_ref_idc: u8,
    pub nal_unit_type: u8,
}

impl FuHeader {
    /// Parse an FU-A header from the first two payload bytes.
    /// Returns `None` if the payload is too short or if forbidden_zero is set.
    pub fn parse(payload: &[u8]) -> Option<Self> {
        if payload.len() < 2 {
            return None;
        }

        let byte1 = payload[0];
        let byte2 = payload[1];

        // Check forbidden_zero: bit 1 of byte1 (the bit after MSB in the NAL header context)
        // Actually, in FU-A payload, the first byte has:
        //   - bit 1 (MSB, after we shift): forbidden_zero - but we already check at NAL level
        // Let me use the standard parsing:
        // Byte 1: D (bit 7) | R (bits 5-6) | Type (bits 0-4)
        // Byte 2: ST (bit 7) | ET (bit 6) | MT (bit 5) | Type upper (bits 0-4)

        // forbidden_zero check: bit 7 of byte1
        if (byte1 & 0x80) != 0 {
            return None;
        }

        let nal_ref_idc = (byte1 >> 5) & 0x03;

        let start = (byte2 >> 7) & 1 == 1;
        let end = (byte2 >> 6) & 1 == 1;

        // The FU-A indicator byte (byte1) carries the full NAL unit type in
        // bits 0-4; the FU header (byte2) merely repeats those 5 bits, so the
        // type comes entirely from byte1.
        let nal_unit_type = byte1 & 0x1F;

        Some(Self {
            start,
            end,
            nal_ref_idc,
            nal_unit_type,
        })
    }

    /// Returns `true` if this FU header carries an IDR NAL unit.
    pub fn is_keyframe_nal(&self) -> bool {
        NalHeader::is_keyframe_nal(self.nal_unit_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nal_header_parse_forbidden_zero_rejection() {
        // forbidden_zero=1 should be rejected (bit 7 = MSB = 0x80)
        assert!(NalHeader::parse_first_byte(0x80).is_none());  // bit 7 set = forbidden_zero
        assert!(NalHeader::parse_first_byte(0x00).is_some());  // forbidden_zero=0
        assert!(NalHeader::parse_first_byte(0x40).is_some());  // bit 6 set, not forbidden_zero
        assert!(NalHeader::parse_first_byte(0x20).is_some());  // bit 5 set, not forbidden_zero
    }

    #[test]
    fn test_nal_header_parse_valid_types() {
        // SLICE_NON_IDR = 1
        assert_eq!(NalHeader::parse_first_byte(0x41).unwrap().nal_unit_type, 1);
        // IDR = 5
        assert_eq!(NalHeader::parse_first_byte(0x45).unwrap().nal_unit_type, 5);
        // SEI = 6
        assert_eq!(NalHeader::parse_first_byte(0x46).unwrap().nal_unit_type, 6);
        // SPS = 7
        assert_eq!(NalHeader::parse_first_byte(0x47).unwrap().nal_unit_type, 7);
        // PPS = 8
        assert_eq!(NalHeader::parse_first_byte(0x48).unwrap().nal_unit_type, 8);
        // AUD = 9
        assert_eq!(NalHeader::parse_first_byte(0x49).unwrap().nal_unit_type, 9);
    }

    #[test]
    fn test_fu_header_parse() {
        // FU-A with start=1, end=0, nal_ref_idc=3, nal_unit_type=1 (SLICE_NON_IDR)
        // byte1: nal_ref_idc=3 in bits 5-6, nal_unit_type_lower=1 in bits 0-4 => 0b_0011_0001 = 0x31
        // Wait: (3 << 5) | 1 = 0x31. Byte1: forbidden_zero=0 (bit7=0), nal_ref_idc=3 (bits 5-6 = 11), type_lower=1 (bits 0-4 = 00001)
        // byte2: start=1 (bit7=1), end=0 (bit6=0), upper_type=0 => 0b_1000_0000 = 0x80
        // The FU-A indicator byte (byte1) carries the full NAL unit type in bits 0-4;
        // the FU header (byte2) merely repeats those 5 bits, so the type comes entirely from byte1.
        let payload = [0x61, 0x80];
        let fu = FuHeader::parse(&payload).unwrap();
        assert!(fu.start);
        assert!(!fu.end);
        assert_eq!(fu.nal_ref_idc, 3);
        assert_eq!(fu.nal_unit_type, 1);

        // FU-A with start=0, end=1, nal_ref_idc=0, nal_unit_type=5 (IDR)
        // byte1: (0 << 5) | 5 = 0x05
        // byte2: start=0 (bit7=0), end=1 (bit6=1), upper=0 => 0b_0100_0000 = 0x40
        let payload2 = [0x05, 0x40];
        let fu2 = FuHeader::parse(&payload2).unwrap();
        assert!(!fu2.start);
        assert!(fu2.end);
        assert_eq!(fu2.nal_ref_idc, 0);
        assert_eq!(fu2.nal_unit_type, 5);
        assert!(fu2.is_keyframe_nal());
    }

    #[test]
    fn test_fu_header_parse_forbidden_zero() {
        // FU with forbidden_zero=1 should be rejected
        // byte1 with MSB set: 0x80
        let payload = [0x80, 0x00];
        assert!(FuHeader::parse(&payload).is_none());
    }

    #[test]
    fn test_fu_header_parse_too_short() {
        assert!(FuHeader::parse(&[0x31]).is_none());
        assert!(FuHeader::parse(&[]).is_none());
    }

    #[test]
    fn test_fu_header_parse_idr_detection() {
        // FU-A carrying IDR: nal_unit_type should be 5
        // Construct: byte1 with type=5 lower bits, byte2 with start=0, end=1
        // nal_unit_type = ((upper << 5) | lower). We need upper and lower to combine to 5.
        // If lower = 5 and upper = 0, then type = 5.
        // byte1: (nal_ref_idc << 5) | 5. With nal_ref_idc=0 => 0x05
        // byte2: start=0, end=1, upper=0 => 0x40
        let payload = [0x05, 0x40];
        let fu = FuHeader::parse(&payload).unwrap();
        assert!(fu.is_keyframe_nal());
    }
}