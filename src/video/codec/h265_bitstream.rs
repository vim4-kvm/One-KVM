const VPS_NAL_TYPE: u8 = 32;
const SPS_NAL_TYPE: u8 = 33;
const PPS_NAL_TYPE: u8 = 34;

/// HEVC payload units, already normalized from HDMI DRM units by V4L2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hdr10Metadata {
    pub mastering: [u8; 24],
    pub cll: [u8; 4],
}

fn escape_rbsp(rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len());
    let mut zeros = 0;
    for &byte in rbsp {
        if zeros == 2 && byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    out
}

fn sei_without_hdr(nal: &[u8]) -> Result<Option<Vec<u8>>, &'static str> {
    let mut rbsp = Vec::new();
    let mut zeros = 0;
    for &byte in &nal[2..] {
        if zeros == 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        rbsp.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    let mut kept = Vec::new();
    let mut cursor = 0;
    while cursor < rbsp.len()
        && !(rbsp[cursor] == 0x80 && rbsp[cursor + 1..].iter().all(|&b| b == 0))
    {
        let start = cursor;
        let mut read_number = || -> Result<usize, &'static str> {
            let mut value = 0usize;
            loop {
                let byte = *rbsp.get(cursor).ok_or("truncated SEI header")?;
                cursor += 1;
                value = value
                    .checked_add(byte as usize)
                    .ok_or("SEI size overflow")?;
                if byte != 255 {
                    return Ok(value);
                }
            }
        };
        let kind = read_number()?;
        let size = read_number()?;
        let end = cursor
            .checked_add(size)
            .filter(|&end| end <= rbsp.len())
            .ok_or("truncated SEI payload")?;
        if kind != 137 && kind != 144 {
            kept.extend_from_slice(&rbsp[start..end]);
        }
        cursor = end;
    }
    if rbsp.get(cursor) != Some(&0x80) || rbsp[cursor + 1..].iter().any(|&v| v != 0) {
        return Err("invalid SEI trailing bits");
    }
    if kept.is_empty() {
        return Ok(None);
    }
    kept.push(0x80);
    let mut result = nal[..2].to_vec();
    result.extend(escape_rbsp(&kept));
    Ok(Some(result))
}

/// Replace static HDR messages while preserving unrelated SEI in mixed NALs.
/// Insert before the first VCL on a random access AU or metadata change.
pub fn with_hdr10(
    data: &[u8],
    metadata: &Hdr10Metadata,
    changed: bool,
) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::with_capacity(data.len() + 80);
    let mut inserted = false;
    let mut error = None;
    let repeat = changed || is_keyframe(data);
    for_each_nal(data, |kind, nal| {
        if kind <= 31 && repeat && !inserted {
            let mut rbsp = vec![137, 24];
            rbsp.extend(metadata.mastering);
            rbsp.extend([144, 4]);
            rbsp.extend(metadata.cll);
            rbsp.push(0x80);
            out.extend([0, 0, 0, 1, 39 << 1, 1]);
            out.extend(escape_rbsp(&rbsp));
            inserted = true;
        }
        if kind == 39 || kind == 40 {
            match sei_without_hdr(nal) {
                Ok(Some(kept)) => {
                    out.extend([0, 0, 0, 1]);
                    out.extend(kept);
                }
                Ok(None) => {}
                Err(e) => error = Some(e),
            }
        } else {
            out.extend([0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
    });
    if let Some(e) = error {
        return Err(e);
    }
    Ok(out)
}

fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut offset = from;
    while offset + 3 <= data.len() {
        if offset + 4 <= data.len() && data[offset..offset + 4] == [0, 0, 0, 1] {
            return Some((offset, 4));
        }
        if data[offset..offset + 3] == [0, 0, 1] {
            return Some((offset, 3));
        }
        offset += 1;
    }
    None
}

fn for_each_nal(data: &[u8], mut visit: impl FnMut(u8, &[u8])) {
    let mut cursor = 0;
    while let Some((start, start_code_len)) = find_start_code(data, cursor) {
        let nal_start = start + start_code_len;
        if nal_start + 2 > data.len() {
            break;
        }
        let next_start = find_start_code(data, nal_start)
            .map(|(offset, _)| offset)
            .unwrap_or(data.len());
        let mut nal_end = next_start;
        while nal_end > nal_start && data[nal_end - 1] == 0 {
            nal_end -= 1;
        }
        if nal_end >= nal_start + 2 {
            visit((data[nal_start] >> 1) & 0x3f, &data[nal_start..nal_end]);
        }
        if next_start == data.len() {
            break;
        }
        cursor = next_start;
    }
}

pub fn is_keyframe(data: &[u8]) -> bool {
    let mut keyframe = false;
    for_each_nal(data, |nal_type, _| {
        if (16..=23).contains(&nal_type) {
            keyframe = true;
        }
    });
    keyframe
}

pub fn extract_vps_sps_pps(data: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;
    for_each_nal(data, |nal_type, nal| match nal_type {
        VPS_NAL_TYPE => vps = Some(nal.to_vec()),
        SPS_NAL_TYPE => sps = Some(nal.to_vec()),
        PPS_NAL_TYPE => pps = Some(nal.to_vec()),
        _ => {}
    });
    (vps, sps, pps)
}

pub fn has_vps_sps_pps(data: &[u8]) -> bool {
    let (vps, sps, pps) = extract_vps_sps_pps(data);
    vps.is_some() && sps.is_some() && pps.is_some()
}

/// The SPS starts with one byte of layer settings, then profile_tier_level.
/// These bytes precede any possible emulation-prevention sequence.
pub fn profile_id(data: &[u8]) -> Option<u8> {
    let (_, sps, _) = extract_vps_sps_pps(data);
    sps.and_then(|sps| sps.get(3).map(|value| value & 31))
}

/// RFC 7798 permits a stream that conforms to the negotiated profile through
/// general_profile_compatibility_flag, even when general_profile_idc differs.
/// In particular our eight-bit Main stream also conforms to Main10, so SDR
/// can retain its efficient encoder within a Main10-capable UU session.
pub fn conforms_to_profile(data: &[u8], profile: u8) -> Option<bool> {
    let (_, sps, _) = extract_vps_sps_pps(data);
    let sps = sps?;
    let mut header = [0u8; 6];
    let (mut count, mut zeros) = (0, 0);
    for byte in sps.into_iter().skip(2) {
        if zeros == 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        header[count] = byte;
        count += 1;
        if count == header.len() {
            break;
        }
        zeros = if byte == 0 { (zeros + 1).min(2) } else { 0 };
    }
    if count != header.len() || profile > 31 {
        return None;
    }
    let compatible = u32::from_be_bytes(header[2..6].try_into().ok()?);
    Some(
        header[1] >> 6 == 0
            && (header[1] & 31 == profile || compatible & (1 << (31 - profile)) != 0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hdr_replacement_preserves_other_sei_and_rejects_truncation() {
        let metadata = Hdr10Metadata {
            mastering: [0; 24],
            cll: [0; 4],
        };
        let mut au = vec![0, 0, 1, 78, 1];
        let mixed = vec![5, 1, 42, 144, 4, 0, 0, 0, 0, 0x80];
        au.extend(escape_rbsp(&mixed));
        au.extend([0, 0, 1, 38, 1, 0x80]);
        let replaced = with_hdr10(&au, &metadata, false).unwrap();
        assert_eq!(with_hdr10(&replaced, &metadata, false).unwrap(), replaced);
        assert!(replaced.windows(3).any(|p| p == [5, 1, 42]));
        assert!(with_hdr10(&[0, 0, 1, 78, 1, 144, 4, 1], &metadata, true).is_err());
    }

    #[test]
    fn changed_metadata_can_replace_sei_before_a_delta_picture() {
        let mut metadata = Hdr10Metadata {
            mastering: [0; 24],
            cll: [0; 4],
        };
        let delta = [0, 0, 0, 1, 2, 1, 0x80];
        assert_eq!(with_hdr10(&delta, &metadata, false).unwrap(), delta);
        let first = with_hdr10(&delta, &metadata, true).unwrap();
        metadata.cll = [3, 232, 0, 0];
        let changed = with_hdr10(&first, &metadata, true).unwrap();
        assert_ne!(changed, first);
        assert_eq!(with_hdr10(&changed, &metadata, true).unwrap(), changed);
    }

    #[test]
    fn profile_compatibility_accepts_main_sdr_in_main10_but_not_the_reverse() {
        let main = [0, 0, 1, 0x42, 1, 1, 1, 0x60, 0, 0, 3, 0, 0x80];
        let main10 = [0, 0, 1, 0x42, 1, 1, 2, 0x20, 0, 0, 3, 0, 0x80];
        assert_eq!(conforms_to_profile(&main, 2), Some(true));
        assert_eq!(conforms_to_profile(&main10, 1), Some(false));
        assert_eq!(conforms_to_profile(&main10, 2), Some(true));
        assert_eq!(conforms_to_profile(&main[..9], 2), None);
        assert_eq!(conforms_to_profile(&main, 32), None);
    }

    #[test]
    fn profile_changes_are_detectable_without_decoding_picture_data() {
        assert_eq!(profile_id(&[0, 0, 1, 0x42, 1, 1, 1, 0x60]), Some(1));
        assert_eq!(profile_id(&[0, 0, 0, 1, 0x42, 1, 1, 2, 0x20]), Some(2));
        assert_eq!(profile_id(&[0, 0, 1, 0x42, 1, 1]), None);
        assert_eq!(profile_id(&[0, 0, 1, 0x26, 1, 0x80]), None);
    }

    #[test]
    fn detects_irap_but_not_trail_frame() {
        assert!(is_keyframe(&[0, 0, 0, 1, 19 << 1, 1, 0xaa]));
        assert!(is_keyframe(&[0, 0, 1, 21 << 1, 1, 0xbb]));
        assert!(!is_keyframe(&[0, 0, 0, 1, 1 << 1, 1, 0xcc]));
    }

    #[test]
    fn extracts_parameter_sets() {
        let data = [
            0,
            0,
            0,
            1,
            32 << 1,
            1,
            0xaa,
            0,
            0,
            1,
            33 << 1,
            1,
            0xbb,
            0,
            0,
            0,
            1,
            34 << 1,
            1,
            0xcc,
        ];
        let (vps, sps, pps) = extract_vps_sps_pps(&data);
        assert_eq!(vps.unwrap(), [32 << 1, 1, 0xaa]);
        assert_eq!(sps.unwrap(), [33 << 1, 1, 0xbb]);
        assert_eq!(pps.unwrap(), [34 << 1, 1, 0xcc]);
        assert!(has_vps_sps_pps(&data));
    }
}
