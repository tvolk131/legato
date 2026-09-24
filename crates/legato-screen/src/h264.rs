//! Just enough H.264 bitstream handling to move frames around: Annex B start codes and
//! NAL unit types.

/// NAL unit types that matter here.
pub mod nal {
    pub const SLICE: u8 = 1;
    pub const IDR: u8 = 5;
    pub const SPS: u8 = 7;
    pub const PPS: u8 = 8;
}

const START: [u8; 4] = [0, 0, 0, 1];

/// Converts length-prefixed NAL units (AVCC, as VideoToolbox produces them) to Annex B,
/// appending to `out`. `len_size` is the size of each length prefix.
pub fn avcc_to_annex_b(avcc: &[u8], len_size: usize, out: &mut Vec<u8>) -> Option<()> {
    let mut rest = avcc;
    while !rest.is_empty() {
        if rest.len() < len_size {
            return None;
        }
        let len = rest[..len_size]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | b as usize);
        rest = &rest[len_size..];
        let unit = rest.get(..len)?;
        out.extend_from_slice(&START);
        out.extend_from_slice(unit);
        rest = &rest[len..];
    }
    Some(())
}

/// Appends one NAL unit with a start code.
pub fn push_nal(unit: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&START);
    out.extend_from_slice(unit);
}

/// The NAL units in an Annex B stream (without their start codes).
pub fn units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let mut end = starts.get(n + 1).map_or(stream.len(), |&next| next - 3);
            // A four-byte start code leaves a zero at the end of the previous unit.
            while end > start && stream[end - 1] == 0 && n + 1 < starts.len() {
                end -= 1;
            }
            &stream[start..end]
        })
        .collect()
}

/// The type of a NAL unit.
pub fn unit_type(unit: &[u8]) -> Option<u8> {
    unit.first().map(|b| b & 0x1f)
}

/// Whether an Annex B access unit starts a new picture sequence (has an IDR slice).
pub fn is_keyframe(stream: &[u8]) -> bool {
    units(stream).iter().any(|u| unit_type(u) == Some(nal::IDR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avcc_becomes_annex_b_and_back() {
        let sps = [0x67, 0x64, 0x00, 0x1f];
        let idr = [0x65, 0x88, 0x84, 0x00, 0x00];
        let mut avcc = Vec::new();
        for unit in [&sps[..], &idr[..]] {
            avcc.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            avcc.extend_from_slice(unit);
        }
        let mut annex_b = Vec::new();
        avcc_to_annex_b(&avcc, 4, &mut annex_b).unwrap();
        assert_eq!(units(&annex_b), [&sps[..], &idr[..]]);
        assert!(is_keyframe(&annex_b));
        assert_eq!(unit_type(&sps), Some(nal::SPS));
    }

    #[test]
    fn truncated_avcc_is_refused() {
        let mut out = Vec::new();
        assert_eq!(avcc_to_annex_b(&[0, 0, 0, 9, 1, 2], 4, &mut out), None);
    }
}
