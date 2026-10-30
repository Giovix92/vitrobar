//! Minute-boundary clock tick.

/// Milliseconds until the next minute boundary; at the boundary itself a full minute.
pub fn ms_until_next_minute(now_ms_into_minute: u32) -> u32 {
    60_000 - now_ms_into_minute % 60_000
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn next_minute() {
        assert_eq!(ms_until_next_minute(0), 60_000);
        assert_eq!(ms_until_next_minute(59_999), 1);
        assert_eq!(ms_until_next_minute(30_000), 30_000);
        assert_eq!(ms_until_next_minute(60_000), 60_000);
        assert_eq!(ms_until_next_minute(61_000), 59_000);
    }
}
