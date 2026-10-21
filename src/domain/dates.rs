use chrono::{Datelike, Duration, NaiveDate, Utc, Weekday};
use chrono_tz::Asia::Vladivostok;

pub fn today_vladivostok() -> NaiveDate {
    Utc::now().with_timezone(&Vladivostok).date_naive()
}

/// Saturday of the upcoming weekend. Saturday stays today; Sunday moves to the next Saturday.
pub fn next_saturday(today: NaiveDate) -> NaiveDate {
    let weekday = today.weekday().num_days_from_monday();
    let days = match weekday {
        5 => 0,
        6 => 6,
        _ => 5 - weekday,
    };
    today + Duration::days(days as i64)
}

pub fn add_days(date: NaiveDate, days: i64) -> NaiveDate {
    date + Duration::days(days)
}

pub fn days_inclusive(start: NaiveDate, end: NaiveDate) -> i64 {
    (end - start).num_days() + 1
}

pub fn format_ru(date: NaiveDate) -> String {
    const MONTHS: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    format!("{} {}", date.day(), MONTHS[(date.month() - 1) as usize])
}

pub fn is_weekend(date: NaiveDate) -> bool {
    matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
}

pub fn parse_month_phrase(query: &str, today: NaiveDate) -> Option<NaiveDate> {
    const MONTHS: [(&str, u32); 12] = [
        ("январ", 1),
        ("феврал", 2),
        ("март", 3),
        ("апрел", 4),
        ("мае", 5),
        ("июн", 6),
        ("июл", 7),
        ("август", 8),
        ("сентябр", 9),
        ("октябр", 10),
        ("ноябр", 11),
        ("декабр", 12),
    ];

    let month = MONTHS
        .iter()
        .find(|(stem, _)| query.contains(stem))
        .map(|(_, month)| *month)?;

    let day = if query.contains("конец") || query.contains("конце") {
        25
    } else if query.contains("середин") {
        15
    } else if query.contains("начал") || query.contains("начале") {
        5
    } else {
        15
    };

    let year = if month < today.month() || (month == today.month() && day < today.day()) {
        today.year() + 1
    } else {
        today.year()
    };
    NaiveDate::from_ymd_opt(year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekend_from_wednesday() {
        let wed = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        assert_eq!(
            next_saturday(wed),
            NaiveDate::from_ymd_opt(2026, 10, 24).unwrap()
        );
    }

    #[test]
    fn weekend_from_sunday_is_next_saturday() {
        let sun = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        assert_eq!(
            next_saturday(sun),
            NaiveDate::from_ymd_opt(2026, 10, 31).unwrap()
        );
    }

    #[test]
    fn month_phrase_rolls_to_next_year() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        let date = parse_month_phrase("в конце ноября", today).unwrap();
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 11, 25).unwrap());
    }
}
