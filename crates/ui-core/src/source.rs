//! Client source lifecycle: the pure logic behind the Workspace repo card, the "Delete source
//! code from this machine" confirmation and the re-fetch dialog (docs/design/2026-10-24_delete-
//! and-refetch-client-source.md, section 6). No rendering framework (RUST-HEADLESS-CORE-1):
//! everything here is unit-tested without a VirtualDom, and the adapter only supplies words and
//! pixels.

/// `24 Oct 2026` from an RFC3339 timestamp (`2026-10-24T17:40:19Z`). Only the date part is read,
/// so any offset or fractional seconds are ignored; anything that does not start with a valid
/// `YYYY-MM-DD` is returned unchanged rather than guessed at.
pub fn format_date(rfc3339: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let date = rfc3339.get(..10).unwrap_or("");
    let mut parts = date.split('-');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return rfc3339.to_string();
    };
    let (Ok(year), Ok(month), Ok(day)) = (y.parse::<u32>(), m.parse::<usize>(), d.parse::<u32>())
    else {
        return rfc3339.to_string();
    };
    if y.len() != 4 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return rfc3339.to_string();
    }
    format!("{day} {} {year}", MONTHS[month - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_read_the_way_the_card_shows_them() {
        assert_eq!(format_date("2026-10-24T17:40:19Z"), "24 Oct 2026");
        assert_eq!(format_date("2026-01-05T00:00:00+02:00"), "5 Jan 2026");
        assert_eq!(format_date("2026-12-31"), "31 Dec 2026");
    }

    #[test]
    fn anything_that_is_not_a_date_is_returned_as_given() {
        assert_eq!(format_date(""), "");
        assert_eq!(format_date("yesterday"), "yesterday");
        assert_eq!(format_date("2026-13-01T00:00:00Z"), "2026-13-01T00:00:00Z");
        assert_eq!(format_date("26-10-24T00:00:00Z"), "26-10-24T00:00:00Z");
    }
}
