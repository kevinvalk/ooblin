//! What AMT hands over encoded, the way a person reads it: GUIDs, times and booleans.

/// SMBIOS keeps a GUID's first three fields little-endian, so the hex AMT gives us reads back to front there.
pub(crate) fn guid(hex: &str) -> String {
    if hex.len() != 32 {
        return hex.to_owned();
    }
    let swapped = |range: std::ops::Range<usize>| -> String {
        hex[range].as_bytes().chunks(2).rev().flat_map(|pair| pair.iter().map(|&byte| byte as char)).collect()
    };
    format!("{}-{}-{}-{}-{}", swapped(0..8), swapped(8..12), swapped(12..16), &hex[16..20], &hex[20..]).to_lowercase()
}

/// AMT's clock and log timestamps count seconds since 1970 in UTC, we show them as `2026-10-18 18:49:51`.
pub(crate) fn utc(seconds: u32) -> String {
    jiff::Timestamp::from_second(seconds.into())
        .map_or_else(|_| seconds.to_string(), |time| time.strftime("%F %T").to_string())
}

/// AMT's `true`, or anything else.
pub(crate) fn yes(value: Option<&str>) -> String {
    if value == Some("true") { "Yes" } else { "No" }.to_owned()
}
