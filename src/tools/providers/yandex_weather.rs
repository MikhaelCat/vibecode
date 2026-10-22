//! Yandex Pogoda (`yandex.ru/pogoda`) as a live weather source.
//!
//! The page is a key-free Next.js app that carries a ten-day forecast for a
//! city. Each day card has a visually-hidden text summary with every number we
//! need: per-part temperatures, wind speed, conditions, sunrise and sunset.
//! There is no open JSON API for the public page and it exposes no
//! precipitation probability, so that value is derived from the condition
//! words exactly like the VL.ru fallback does; the raw condition is kept in
//! `description`, and a day without temperatures is dropped instead of guessed.

use std::time::Duration;

use chrono::{Datelike, NaiveDate};
use serde_json::{json, Value};

use crate::domain::dates::today_vladivostok;
use crate::domain::plan::parse_time;
use crate::tools::registry::ToolCtx;

/// Yandex serves the page only to a browser-like client; the default reqwest
/// user agent gets a stripped-down document.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Fetches the ten-day Yandex Pogoda forecast for the settlement nearest to the
/// coordinates and returns the days inside `[start, end]` plus the city name.
pub async fn forecast(
    ctx: &ToolCtx,
    lat: f64,
    lon: f64,
    start: NaiveDate,
    end: NaiveDate,
) -> Option<(Vec<Value>, String)> {
    let settlement = crate::domain::settlements::nearest(lat, lon)?;
    let slug = yandex_slug(&settlement.name);
    let url = format!("https://yandex.ru/pogoda/ru/{slug}");
    let response = tokio::time::timeout(
        Duration::from_secs(6),
        ctx.http
            .get(&url)
            .header("User-Agent", USER_AGENT)
            .header("accept-language", "ru,en;q=0.9")
            .send(),
    )
    .await
    .ok()?
    .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let html = response.text().await.ok()?;
    let days = parse_days(&html, today_vladivostok(), start, end);
    if days.is_empty() {
        None
    } else {
        Some((days, settlement.name.clone()))
    }
}

/// Transliterates a Primorye settlement name into a Yandex Pogoda slug
/// (`Владивосток` → `vladivostok`, `Спасск-Дальний` → `spassk-dalniy`).
/// Cities whose slug does not match the transliteration simply fall through to
/// the next weather source.
fn yandex_slug(name: &str) -> String {
    let mut slug = String::new();
    for character in name.to_lowercase().chars() {
        let latin: &str = match character {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'ё' => "e",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "kh",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ъ' | 'ь' => "",
            'ы' => "y",
            'э' => "e",
            'ю' => "yu",
            'я' => "ya",
            // "Спасск-Дальний" and "Большой Камень" keep their separator as a
            // single dash; every other non-letter is dropped.
            '-' | ' ' | '_' => {
                if !slug.is_empty() && !slug.ends_with('-') {
                    slug.push('-');
                }
                continue;
            }
            other if other.is_ascii_alphanumeric() => {
                slug.push(other);
                continue;
            }
            _ => continue,
        };
        slug.push_str(latin);
    }
    slug.trim_matches('-').to_string()
}

/// Parses the day cards of the loaded page. `today` is the same Vladivostok
/// date the rest of the app uses, because the page labels its first card with
/// the city's own "Сегодня".
fn parse_days(html: &str, today: NaiveDate, start: NaiveDate, end: NaiveDate) -> Vec<Value> {
    let mut days: Vec<Value> = Vec::new();
    let mut seen: Vec<NaiveDate> = Vec::new();
    for chunk in html.split("AppForecastDay_dayCard").skip(1) {
        let Some(summary) = hidden_summary(chunk) else {
            continue;
        };
        let Some(date) = card_date(&summary, today) else {
            continue;
        };
        if seen.contains(&date) || date < start || date > end {
            continue;
        }
        let Some(day) = parse_summary(&summary, date) else {
            continue;
        };
        // The page repeats the same cards in its RSC payload; the first pass
        // wins so a day is never counted twice.
        seen.push(date);
        days.push(day);
    }
    days.sort_by_key(|day| {
        day.get("date")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    });
    days
}

/// Text of the first visually-hidden paragraph of a day card.
fn hidden_summary(chunk: &str) -> Option<String> {
    let marker = "visuallyHidden__y0sw0 visuallyHidden\">";
    let start = chunk.find(marker)? + marker.len();
    let rest = &chunk[start..];
    let end = rest.find("</p>")?;
    Some(rest[..end].to_string())
}

/// "Сегодня, 22 октября" / "Завтра, 23 октября" / "Суббота, 24 октября" → date.
fn card_date(summary: &str, today: NaiveDate) -> Option<NaiveDate> {
    let label = summary.split('<').next()?.trim_end_matches(':').trim();
    let (kind, rest) = label.split_once(',')?;
    let kind = kind.trim();
    if kind.eq_ignore_ascii_case("сегодня") {
        return Some(today);
    }
    if kind.eq_ignore_ascii_case("завтра") {
        return Some(today + chrono::Duration::days(1));
    }
    let mut words = rest.split_whitespace();
    let day: u32 = words.next()?.parse().ok()?;
    let month = month_number(words.next()?)?;
    // Weekday labels carry the day and month; resolve them forward from today
    // so a ten-day window never wraps to last month.
    (0..14).find_map(|offset| {
        let date = today + chrono::Duration::days(offset);
        (date.day() == day && date.month() == month).then_some(date)
    })
}

fn month_number(name: &str) -> Option<u32> {
    let name = name.to_lowercase();
    const MONTHS: [(&str, u32); 12] = [
        ("январ", 1),
        ("феврал", 2),
        ("март", 3),
        ("апрел", 4),
        ("ма", 5),
        ("июн", 6),
        ("июл", 7),
        ("август", 8),
        ("сентябр", 9),
        ("октябр", 10),
        ("ноябр", 11),
        ("декабр", 12),
    ];
    MONTHS
        .iter()
        .find(|(needle, _)| name.starts_with(*needle))
        .map(|(_, month)| *month)
}

/// Turns the hidden summary of one day into a `WeatherDay`-shaped value.
fn parse_summary(summary: &str, date: NaiveDate) -> Option<Value> {
    let mut temperatures: Vec<f64> = Vec::new();
    let mut wind_max: f64 = 0.0;
    let mut conditions: Vec<String> = Vec::new();
    let mut sunrise = None;
    let mut sunset = None;

    for part in summary.split("<br />") {
        let part = part.trim();
        let is_day_part = ["утром", "днём", "вечером", "ночью"]
            .iter()
            .any(|word| part.starts_with(word));
        if is_day_part {
            if let Some(temperature) = part_temperature(part) {
                temperatures.push(temperature);
            }
            if let Some(wind) = number_between(part, "скорость ветра ", " м/с") {
                wind_max = wind_max.max(wind);
            }
            if let Some(condition) = condition_of(part) {
                if !conditions.contains(&condition) {
                    conditions.push(condition);
                }
            }
        } else if let Some(time) = part
            .strip_prefix("восход ")
            .and_then(|text| text.get(..5))
            .and_then(parse_time)
        {
            sunrise = Some(time);
        } else if let Some(time) = part
            .strip_prefix("закат ")
            .and_then(|text| text.get(..5))
            .and_then(parse_time)
        {
            sunset = Some(time);
        }
    }

    if temperatures.len() < 2 {
        // Without real temperatures the day must not become "+0 и сухо".
        return None;
    }
    let temp_min = temperatures.iter().cloned().fold(f64::INFINITY, f64::min);
    let temp_max = temperatures
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    let description = conditions.join(", ");
    let precip_prob = precipitation_probability(&description);

    Some(json!({
        "date": date,
        "precip_prob": precip_prob,
        "wind_max": wind_max,
        "temp_min": temp_min,
        "temp_max": temp_max,
        "sunrise": sunrise.map(|time| time.format("%H:%M:%S").to_string()),
        "sunset": sunset.map(|time| time.format("%H:%M:%S").to_string()),
        "description": description,
        "source": "yandex-pogoda"
    }))
}

/// "утром температура воздуха +5°, …" and "днём +9°, …" both start their
/// temperature with a signed number.
fn part_temperature(part: &str) -> Option<f64> {
    let tail = part
        .split_once("температура воздуха ")
        .map(|(_, tail)| tail)
        .or_else(|| part.split_once(' ').map(|(_, tail)| tail))?;
    let sign_index = tail.find(['+', '−', '-'])?;
    let tail = &tail[sign_index..];
    let end = tail
        .find(|character: char| {
            !character.is_ascii_digit() && character != '+' && character != '−' && character != '-'
        })
        .unwrap_or(tail.len());
    parse_signed(&tail[..end])
}

fn parse_signed(value: &str) -> Option<f64> {
    let normalized = value.replace('−', "-").replace('+', "");
    normalized.trim().parse::<f64>().ok()
}

fn number_between(text: &str, prefix: &str, suffix: &str) -> Option<f64> {
    let rest = text.split_once(prefix)?.1;
    let value = rest
        .split_once(suffix)
        .map(|(value, _)| value)
        .or_else(|| {
            // "скорость ветра 5,3 м/с," and "скорость ветра 2 м/с,"
            rest.split(',').next()
        })?;
    value.trim().replace(',', ".").parse::<f64>().ok()
}

/// Text between the "ощущается как ±N°, " and ", скорость ветра" markers.
fn condition_of(part: &str) -> Option<String> {
    // "днём +14°, ощущается как +12°, небольшой дождь, скорость ветра 3,5 м/с,
    // южный, влажность 75%…" — the condition is the field right before the wind.
    let rest = part
        .split_once("скорость ветра")?
        .0
        .trim_end_matches(',')
        .trim();
    let tail = rest.rsplit_once(", ").map(|(_, tail)| tail).unwrap_or(rest);
    let condition = tail.trim().trim_end_matches(',').trim().to_string();
    if condition.is_empty() {
        None
    } else {
        Some(condition)
    }
}

/// The page has no precipitation figure. A condition that names precipitation
/// is reported as a likely one (the app warns outdoor stops from 70%), every
/// other condition as dry — the same convention as the VL.ru fallback.
fn precipitation_probability(description: &str) -> f64 {
    let description = description.to_lowercase();
    if ["дожд", "ливн", "снег", "гроза", "морос", "град"]
        .iter()
        .any(|needle| description.contains(needle))
    {
        80.0
    } else {
        0.0
    }
}

/// Convenience wrapper used by the tool: keeps the city label for the summary.
pub fn summary_label(city: &str) -> String {
    format!("Яндекс.Погода: прогноз по {city}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real markup of two day cards (today and a weekend card), trimmed to the
    /// parts the parser reads.
    const PAGE: &str = r#"<ul class="MainPage_appForecast">
<li class="AppForecastDay_dayCard__rIRAn AppForecast_container__day__b3xlV"><p class="A11Y_visuallyHidden__y0sw0 visuallyHidden">Сегодня, 22 октября:<br />утром температура воздуха +5°, ощущается как −3°, ясно, скорость ветра 7,4 м/с, северо-западный, влажность 50%, давление 767 мм рт. ст.<br />днём +9°, ощущается как +3°, ясно, скорость ветра 5,3 м/с, северо-западный, влажность 36%, давление 768 мм рт. ст.<br />вечером +9°, ощущается как +4°, ясно, скорость ветра 4 м/с, южный, влажность 46%, давление 768 мм рт. ст.<br />ночью +9°, ощущается как +4°, малооблачно, скорость ветра 4,6 м/с, юго-восточный, влажность 53%, давление 769 мм рт. ст.<br />уф-индекс: 3, умеренный<br />температура воды днём +13°<br />восход 07:35<br />закат 18:19<br />световой день: 10 ч 44 мин</p><article data-day="1_22"></article></li>
<li class="AppForecastDay_dayCard__rIRAn AppForecast_container__day__b3xlV"><p class="A11Y_visuallyHidden__y0sw0 visuallyHidden">Суббота, 24 октября:<br />утром температура воздуха +11°, ощущается как +8°, малооблачно, скорость ветра 2,4 м/с, западный, влажность 60%, давление 763 мм рт. ст.<br />днём +14°, ощущается как +12°, небольшой дождь, скорость ветра 3,5 м/с, южный, влажность 75%, давление 762 мм рт. ст.<br />вечером +14°, ощущается как +9°, облачно с прояснениями, скорость ветра 4,4 м/с, юго-западный, влажность 42%, давление 762 мм рт. ст.<br />ночью +12°, ощущается как +6°, малооблачно, скорость ветра 6,5 м/с, северный, влажность 59%, давление 764 мм рт. ст.<br />восход 07:37<br />закат 18:16<br />световой день: 10 ч 39 мин</p><article data-day="3_24"></article></li>
</ul>"#;

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
    }

    #[test]
    fn transliterates_settlement_names_into_slugs() {
        assert_eq!(yandex_slug("Владивосток"), "vladivostok");
        assert_eq!(yandex_slug("Находка"), "nakhodka");
        assert_eq!(yandex_slug("Уссурийск"), "ussuriysk");
        assert_eq!(yandex_slug("Артём"), "artem");
        assert_eq!(yandex_slug("Спасск-Дальний"), "spassk-dalniy");
        assert_eq!(yandex_slug("Большой Камень"), "bolshoy-kamen");
        assert_eq!(yandex_slug("Камень-Рыболов"), "kamen-rybolov");
    }

    #[test]
    fn parses_real_day_cards() {
        let days = parse_days(PAGE, date(22), date(22), date(25));
        assert_eq!(days.len(), 2);
        let first = &days[0];
        assert_eq!(first["date"].as_str(), Some("2026-10-22"));
        assert_eq!(first["temp_min"].as_f64(), Some(5.0));
        assert_eq!(first["temp_max"].as_f64(), Some(9.0));
        assert_eq!(first["wind_max"].as_f64(), Some(7.4));
        assert_eq!(first["precip_prob"].as_f64(), Some(0.0));
        assert_eq!(first["sunrise"].as_str(), Some("07:35:00"));
        assert_eq!(first["sunset"].as_str(), Some("18:19:00"));

        let rainy = &days[1];
        assert_eq!(rainy["date"].as_str(), Some("2026-10-24"));
        assert_eq!(rainy["temp_min"].as_f64(), Some(11.0));
        assert_eq!(rainy["temp_max"].as_f64(), Some(14.0));
        assert_eq!(rainy["precip_prob"].as_f64(), Some(80.0));
        assert!(rainy["description"]
            .as_str()
            .unwrap()
            .contains("небольшой дождь"));
    }

    #[test]
    fn filters_days_outside_the_requested_window() {
        let days = parse_days(PAGE, date(22), date(23), date(23));
        assert!(days.is_empty(), "22.10 must be filtered out, 24.10 too");
        let days = parse_days(PAGE, date(22), date(22), date(22));
        assert_eq!(days.len(), 1);
    }

    #[test]
    fn ignores_a_card_without_temperatures() {
        let broken = r#"<li class="AppForecastDay_dayCard"><p class="A11Y_visuallyHidden__y0sw0 visuallyHidden">Сегодня, 22 октября:<br />восход 07:35<br />закат 18:19</p></li>"#;
        assert!(parse_days(broken, date(22), date(22), date(23)).is_empty());
    }

    #[test]
    fn resolves_weekday_labels_forward() {
        // The page was rendered on 28 October and the card says "Суббота, 31".
        assert_eq!(card_date("Суббота, 31 октября:", date(28)), Some(date(31)));
        assert_eq!(card_date("Сегодня, 22 октября:", date(22)), Some(date(22)));
        assert_eq!(card_date("Завтра, 23 октября:", date(22)), Some(date(23)));
    }
}
