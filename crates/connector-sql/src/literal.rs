//! Standard SQL spellings that SQL dialects build their identifiers and
//! literals from.

/// Quotes an identifier with double quotes, doubling embedded quotes.
pub fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Quotes a string literal with single quotes, doubling embedded quotes.
///
/// Backslashes are ordinary characters, as in standard SQL.
pub fn quote_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Writes `CAST(<value> AS <type_name>)`.
pub fn cast(value: &str, type_name: &str) -> String {
    format!("CAST({value} AS {type_name})")
}

/// Writes an unscaled decimal at `scale` in plain decimal notation, such as
/// `-0.005` or, for a negative scale, `1200`.
pub fn decimal_text(unscaled: i128, scale: i8) -> String {
    let digits = unscaled.unsigned_abs().to_string();
    let sign = if unscaled < 0 { "-" } else { "" };
    if scale <= 0 {
        let zeros = "0".repeat(usize::from(scale.unsigned_abs()));
        return format!("{sign}{digits}{zeros}");
    }
    let scale = usize::from(scale.unsigned_abs());
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (integer, fraction) = padded.split_at(padded.len() - scale);
    format!("{sign}{integer}.{fraction}")
}

/// Microseconds in one day.
pub const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Converts days from 1970-01-01 to a proleptic Gregorian (year, month, day),
/// where year 0 is 1 BC.
///
/// This is Howard Hinnant's `civil_from_days`.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Formats microseconds from midnight as `HH:MM:SS.ffffff`.
pub fn time_text(micros: i64) -> String {
    let seconds = micros / 1_000_000;
    format!(
        "{:02}:{:02}:{:02}.{:06}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60,
        micros % 1_000_000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_doubles_embedded_quotes() {
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_string("it's \\"), "'it''s \\'");
    }

    #[test]
    fn decimals_keep_their_exact_digits() {
        assert_eq!(decimal_text(-5, 3), "-0.005");
        assert_eq!(decimal_text(12, -2), "1200");
        assert_eq!(decimal_text(1250, 2), "12.50");
    }

    #[test]
    fn days_convert_to_proleptic_gregorian_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(-719_162), (1, 1, 1));
        assert_eq!(civil_from_days(-719_528), (0, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn times_of_day_have_microsecond_fractions() {
        assert_eq!(time_text(45_296_000_007), "12:34:56.000007");
    }
}
