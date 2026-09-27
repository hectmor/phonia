//! Turning wire values into short text, shared by every client that prints them.

/// `1:30`, or `1:02:05` from an hour on.
pub fn ms(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_below_an_hour_seconds_at_two_digits_and_hours_from_3600() {
        assert_eq!(ms(0), "0:00");
        assert_eq!(ms(65_000), "1:05");
        assert_eq!(ms(3_599_000), "59:59");
        assert_eq!(ms(3_600_000), "1:00:00");
        assert_eq!(ms(3_723_000), "1:02:03");
    }
}
