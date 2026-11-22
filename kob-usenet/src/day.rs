use std::time::{SystemTime, UNIX_EPOCH};

pub const SECONDS: i64 = 86_400;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

pub fn today() -> i64 {
    of_unix(now())
}

pub const fn of_unix(secs: i64) -> i64 {
    secs.div_euclid(SECONDS)
}

pub fn parse(iso: &str) -> Option<i64> {
    let mut parts = iso.trim().splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then(|| civil(year, month, day))
}

const fn civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    #[test]
    fn iso_dates_count_days_since_the_epoch() {
        assert_eq!(super::parse("1970-01-01"), Some(0));
        assert_eq!(super::parse("2000-03-01"), Some(11_017));
        assert_eq!(
            super::parse("2026-11-22"),
            Some(super::of_unix(1_795_305_600))
        );
        assert_eq!(super::parse("2026-13-01"), None);
        assert_eq!(super::parse("soon"), None);
    }
}
