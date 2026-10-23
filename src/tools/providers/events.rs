//! VL.ru events parser.
//!
//! The city poster exposes schema.org `Event` objects as JSON-LD, so this
//! provider parses structured data instead of scraping markup. Events are
//! filtered to the trip window, deduplicated by title/date/place and returned
//! with `live` provenance; upstream failures surface as `ToolError::Upstream`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

const EVENTS_USER_AGENT: &str =
    "Mozilla/5.0 (compatible; vibecode-travel-ai/0.1; +https://github.com/MikhaelCat/vibecode)";

fn events_url(area: &str) -> Option<&'static str> {
    let area = area.to_lowercase();
    if area.is_empty()
        || area.contains("владивост")
        || area.contains("русск")
        || area.contains("примор")
    {
        Some("https://www.vl.ru/afisha/vladivostok")
    } else if area.contains("находк") {
        Some("https://www.vl.ru/afisha/nakhodka")
    } else if area.contains("уссурийск") || area.contains("ussuriisk") {
        Some("https://www.vl.ru/afisha/ussuriisk")
    } else if area.contains("больш") && area.contains("камен") {
        Some("https://www.vl.ru/afisha/bolshoy-kamen")
    } else if area.contains("артем") || area.contains("артём") || area.contains("artem") {
        Some("https://www.vl.ru/afisha/artem")
    } else if area.contains("спасск") {
        Some("https://www.vl.ru/afisha/spassk")
    } else if area.contains("партизанск") {
        Some("https://www.vl.ru/afisha/partizansk")
    } else if area.contains("арсеньев") {
        Some("https://www.vl.ru/afisha/arsenyev")
    } else if area.contains("дальнегорск") {
        Some("https://www.vl.ru/afisha/dalnegorsk")
    } else if area.contains("лесозаводск") {
        Some("https://www.vl.ru/afisha/lesozavodsk")
    } else if area.contains("фокино") {
        Some("https://www.vl.ru/afisha/fokino")
    } else if area.contains("кавалерово") {
        Some("https://www.vl.ru/afisha/kavalerovo")
    } else if area.contains("черниговка") {
        Some("https://www.vl.ru/afisha/chernigovka")
    } else {
        None
    }
}

/// Live events from the VL.ru city poster. The page exposes schema.org Event
/// objects as JSON-LD, so we parse structured data instead of scraping markup.
pub struct EventsTool;

#[async_trait]
impl Tool for EventsTool {
    fn name(&self) -> &'static str {
        "find_events"
    }

    fn description(&self) -> &'static str {
        "Живые события и мероприятия из афиши VL.ru на даты поездки: Владивосток, Находка, Уссурийск, Большой Камень, Артём, Спасск, Партизанск, Арсеньев, Дальнегорск, Лесозаводск, Фокино, Кавалерово, Черниговка."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "area": {"type": "string"},
                "start_date": {"type": "string", "description": "YYYY-MM-DD"},
                "end_date": {"type": "string", "description": "YYYY-MM-DD"}
            },
            "required": ["area", "start_date", "end_date"]
        })
    }

    /// Fetches the poster and returns at most 12 events overlapping the
    /// requested dates. Missing date arguments default to the intent window.
    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let area = args
            .get("area")
            .and_then(Value::as_str)
            .unwrap_or("Владивосток");
        // The poster exists for Vladivostok, Nakhodka and Arsenyev. Returning
        // Vladivostok events for a trip to Ussuriysk would put the wrong city
        // in the plan.
        let Some(url) = events_url(area) else {
            return Ok(ToolOutput::ok(
                json!({"events": [], "area": area}),
                SourceKind::Stale,
                format!(
                    "VL.ru: афиша доступна для Владивостока, Находки и Арсеньева — для «{area}» событий нет"
                ),
            ));
        };
        let start = parse_date_arg(&args, "start_date").unwrap_or(ctx.intent.start_date);
        let end = parse_date_arg(&args, "end_date").unwrap_or(ctx.intent.end_date);
        if end < start {
            return Err(ToolError::BadArgs(
                "end_date не может быть раньше start_date".to_string(),
            ));
        }

        // The poster page and its AJAX pages are independent requests: start
        // them all at once. Waiting for the main page first meant the extra
        // pages only began after the slow HTML document (>6 s on a slow link)
        // had fully arrived, and the poster is on the plan's critical path.
        let requests = (1..=5).map(|page| {
            let ajax_url = format!("{url}/ajax/events?page={page}");
            let http = ctx.http.clone();
            async move {
                let response = tokio::time::timeout(
                    Duration::from_secs(4),
                    http.get(&ajax_url)
                        .header("User-Agent", EVENTS_USER_AGENT)
                        .header("Accept", "application/json")
                        .send(),
                )
                .await
                .ok()?
                .ok()?;
                if !response.status().is_success() {
                    return None;
                }
                response.json::<Value>().await.ok()
            }
        });
        let (page, pages) = tokio::join!(
            async {
                let response = ctx
                    .http
                    .get(url)
                    .header("User-Agent", EVENTS_USER_AGENT)
                    .header("Accept", "text/html,application/xhtml+xml")
                    .send()
                    .await
                    .map_err(|error| ToolError::Upstream(format!("VL.ru: {error}")))?;
                if !response.status().is_success() {
                    return Err(ToolError::Upstream(format!(
                        "VL.ru вернул HTTP {}",
                        response.status()
                    )));
                }
                response
                    .text()
                    .await
                    .map_err(|error| ToolError::Upstream(format!("VL.ru: {error}")))
            },
            futures::future::join_all(requests),
        );
        let html = page?;

        let mut events = parse_events(&html, start, end);
        // The first page is only a subset. Pull a few AJAX pages so a busy
        // Vladivostok day can show up to 20 events, not 8-10.
        let mut seen: HashSet<String> = events
            .iter()
            .filter_map(|event| event.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        for value in pages.into_iter().flatten() {
            let Some(page_html) = value.get("events").and_then(Value::as_str) else {
                continue;
            };
            for event in parse_events(page_html, start, end) {
                let id = event
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if id.is_empty() || seen.insert(id) {
                    events.push(event);
                }
            }
        }
        events.truncate(200);
        for event in &mut events {
            if let Some(object) = event.as_object_mut() {
                object.insert("area".to_string(), json!(area));
            }
        }
        let summary = if events.is_empty() {
            format!("VL.ru: событий на {} — {} не найдено", start, end)
        } else {
            format!("VL.ru: {} событий на даты поездки ({area})", events.len())
        };
        Ok(ToolOutput::ok(
            json!({"events": events, "area": area}),
            SourceKind::Live,
            summary,
        ))
    }
}

fn parse_date_arg(args: &Value, key: &str) -> Option<NaiveDate> {
    args.get(key)
        .and_then(Value::as_str)
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
}

/// Extracts events from every JSON-LD block, keeps those overlapping
/// `start..=end`, deduplicates them and sorts by start date.
///
/// JSON-LD отдаёт только дату (`startDate` без времени), поэтому время
/// начала берётся из той же разметки афиши по названию события.
fn parse_events(html: &str, start: NaiveDate, end: NaiveDate) -> Vec<Value> {
    let times = html_event_times(html);
    let mut events = Vec::new();
    let mut seen = HashSet::new();
    for block in json_ld_blocks(html) {
        let Ok(value) = serde_json::from_str::<Value>(&block) else {
            continue;
        };
        let items = match value {
            Value::Array(items) => items,
            object => vec![object],
        };
        for item in items {
            let Some(event) = event_from_json(&item) else {
                continue;
            };
            let (Some(event_start), Some(event_end)) = (event.start, event.end) else {
                continue;
            };
            if event_start > end || event_end < start {
                continue;
            }
            let key = format!("{}|{}|{}", event.title, event_start, event.place);
            if !seen.insert(key) {
                continue;
            }
            let mut value = json!({
                "id": event.id,
                "title": event.title,
                "date": event_start,
                "end_date": event_end,
                "place": event.place,
                "url": event.url,
                "score": event.score,
                "source": "live"
            });
            if let Some(time) = times.get(&normalize_event_title(&event.title)) {
                if let Some(object) = value.as_object_mut() {
                    object.insert("start_time".to_string(), json!(time));
                }
            }
            events.push(value);
        }
    }
    events.sort_by(|a, b| {
        let left = a.get("date").and_then(Value::as_str).unwrap_or("");
        let right = b.get("date").and_then(Value::as_str).unwrap_or("");
        left.cmp(right)
    });
    events
}

/// Разметка одной карточки события занимает пару сотен байт. Окно сканирования
/// не даёт случайно схватить заголовок или дату соседней карточки.
const CARD_SCAN_WINDOW: usize = 1200;

/// Пары «нормализованное название -> время начала» из HTML афиши.
///
/// `event-list__item-date` (`25 октября, 10:00` или `3 мар. 2027, 19:00`)
/// стоит рядом с заголовком карточки; у событий «на весь день» времени в
/// блоке нет — тогда пары не будет. Страницы `/ajax/events?page=N` содержат
/// ту же разметку, поэтому извлекатель работает и по ним.
fn html_event_times(html: &str) -> HashMap<String, String> {
    let mut times = HashMap::new();
    let mut rest = html;
    while let Some(found) = rest.find("event-list__item-title") {
        rest = &rest[found + "event-list__item-title".len()..];
        let Some((title, tail)) = title_span(rest) else {
            continue;
        };
        let window = head_bytes(tail, CARD_SCAN_WINDOW);
        let Some(date_at) = window.find("event-list__item-date") else {
            continue;
        };
        // Заголовок следующей карточки идёт раньше даты: у этой карточки блока
        // с датой нет, чужое время брать нельзя.
        if window
            .find("event-list__item-title")
            .is_some_and(|next_title| next_title < date_at)
        {
            continue;
        }
        let Some(time) = first_time_token(date_block_text(&tail[date_at..])) else {
            continue;
        };
        let key = normalize_event_title(title);
        if !key.is_empty() {
            times.entry(key).or_insert(time);
        }
    }
    times
}

/// Содержимое `<span>` в заголовке карточки:
/// `<h3 class="event-list__item-title"><span>НАЗВАНИЕ</span></h3>`.
/// Возвращает название и остаток разметки сразу за ним.
fn title_span(after_marker: &str) -> Option<(&str, &str)> {
    let window = head_bytes(after_marker, CARD_SCAN_WINDOW);
    let open = window.find("<span")?;
    let text_start = open + window[open..].find('>')? + 1;
    let rest = &after_marker[text_start..];
    let close = head_bytes(rest, CARD_SCAN_WINDOW).find("</span>")?;
    let title = rest[..close].trim();
    if title.is_empty() {
        return None;
    }
    Some((title, &rest[close..]))
}

/// Текст блока с датой без вложенной разметки («и ещё N сеансов» лежит в
/// отдельном `<span>`, и в нём времени нет).
fn date_block_text(date_html: &str) -> &str {
    let Some(tag_end) = date_html.find('>') else {
        return "";
    };
    let rest = &date_html[tag_end + 1..];
    match rest.find("</div>") {
        Some(close) => &rest[..close],
        None => rest,
    }
}

/// Первое `ЧЧ:ММ` в тексте блока с датой. Часы приводятся к двум цифрам,
/// чтобы «17:00» и «19:00» сравнивались как строки напрямую.
fn first_time_token(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for colon in 0..bytes.len() {
        if bytes[colon] != b':' || colon == 0 {
            continue;
        }
        if colon + 3 > bytes.len()
            || !bytes[colon + 1].is_ascii_digit()
            || !bytes[colon + 2].is_ascii_digit()
        {
            continue;
        }
        let mut start = colon;
        while start > 0 && bytes[start - 1].is_ascii_digit() && colon - start < 2 {
            start -= 1;
        }
        if start == colon {
            continue;
        }
        let (Ok(hour), Ok(minute)) = (
            text[start..colon].parse::<u32>(),
            text[colon + 1..colon + 3].parse::<u32>(),
        ) else {
            continue;
        };
        if hour > 23 || minute > 59 {
            continue;
        }
        return Some(format!("{hour:02}:{minute:02}"));
    }
    None
}

/// Название для сопоставления JSON-LD и разметки: регистр, пробелы, ё,
/// кавычки и HTML-сущности не должны мешать совпадению.
fn normalize_event_title(raw: &str) -> String {
    raw.replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&laquo;", "«")
        .replace("&raquo;", "»")
        .replace("&nbsp;", " ")
        .replace("&thinsp;", " ")
        .replace("&ndash;", "-")
        .replace("&mdash;", "-")
        .replace("&amp;", "&")
        .to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Первые `max` байт строки, обрезанные по границе UTF-8 символа.
fn head_bytes(text: &str, max: usize) -> &str {
    match text.char_indices().find(|(index, _)| *index >= max) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// Intermediate event record parsed from one schema.org object.
struct ParsedEvent {
    id: String,
    title: String,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    place: String,
    url: Option<String>,
    score: f64,
}

/// Reads one schema.org `Event`; requires a title and a parseable start date.
/// The end date falls back to the start date for one-day events.
fn event_from_json(value: &Value) -> Option<ParsedEvent> {
    let is_event = match value.get("@type") {
        Some(Value::String(kind)) => kind == "Event",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind.as_str() == Some("Event")),
        _ => false,
    };
    if !is_event {
        return None;
    }
    let title = value
        .get("name")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    let start = parse_iso_date(value.get("startDate").and_then(Value::as_str));
    let end = parse_iso_date(value.get("endDate").and_then(Value::as_str)).or(start);
    let place = value
        .get("location")
        .and_then(|location| {
            location
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| location.get("address").and_then(Value::as_str))
        })
        .unwrap_or("Владивосток")
        .trim()
        .to_string();
    let url = value
        .get("url")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("location")
                .and_then(|location| location.get("url"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    let id = url.clone().unwrap_or_else(|| {
        format!(
            "vl:event:{}:{}",
            start.map(|d| d.to_string()).unwrap_or_default(),
            title
        )
    });
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut score = (description.chars().count().min(300) as f64) / 100.0;
    if value.get("image").is_some() {
        score += 1.0;
    }
    if value
        .get("location")
        .and_then(|location| location.get("url"))
        .is_some()
    {
        score += 0.5;
    }
    if let Some(rating) = value
        .get("aggregateRating")
        .and_then(|rating| rating.get("ratingValue"))
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
    {
        score += rating;
    }
    let place_lower = place.to_lowercase();
    if [
        "филармония",
        "театр",
        "концерт холл",
        "fesco",
        "приморская сцена",
        "опера",
        "балет",
        "арена",
        "цирк",
    ]
    .iter()
    .any(|word| place_lower.contains(word))
    {
        score += 3.0;
    }
    let title_lower = title.to_lowercase();
    if [
        "премьер",
        "фестивал",
        "концерт",
        "спектакл",
        "выставк",
        "стендап",
        "турнир",
    ]
    .iter()
    .any(|word| title_lower.contains(word))
    {
        score += 1.0;
    }
    Some(ParsedEvent {
        id,
        title,
        start,
        end,
        place,
        url,
        score,
    })
}

fn parse_iso_date(value: Option<&str>) -> Option<NaiveDate> {
    let value = value?;
    let date = value.get(..10)?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

/// Returns the raw contents of every `application/ld+json` script tag.
fn json_ld_blocks(html: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = html;
    while let Some(script_start) = rest.find("application/ld+json") {
        rest = &rest[script_start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        rest = &rest[tag_end + 1..];
        let Some(script_end) = rest.find("</script>") else {
            break;
        };
        blocks.push(rest[..script_end].trim().to_string());
        rest = &rest[script_end + 9..];
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_ld_event_and_filters_dates() {
        let html = r#"
        <script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Event","name":"Концерт",
         "startDate":"2026-10-24","endDate":"2026-10-24",
         "location":{"@type":"Place","name":"Концерт Холл"},"url":"https://www.vl.ru/afisha/vladivostok/event/1"}
        </script>
        <script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Event","name":"Позже",
         "startDate":"2026-12-19","endDate":"2026-12-19","location":{"name":"Арена"}}
        </script>
        "#;
        let start = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let events = parse_events(html, start, end);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["title"], "Концерт");
        assert_eq!(events[0]["place"], "Концерт Холл");
        assert_eq!(events[0]["date"], "2026-10-24");
        assert_eq!(events[0]["source"], "live");
    }

    #[test]
    fn deduplicates_same_event() {
        let block = r#"<script type="application/ld+json">
        {"@type":"Event","name":"Спектакль","startDate":"2026-10-24","location":{"name":"Театр"}}
        </script>"#;
        let html = format!("{block}{block}");
        let date = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        assert_eq!(parse_events(&html, date, date).len(), 1);
    }

    /// Время лежит в HTML рядом с заголовком: обе формы записи даты и событие
    /// «на весь день» без времени.
    #[test]
    fn reads_times_from_poster_markup() {
        let html = r#"
        <li class="event-list__item">
          <h3 class="event-list__item-title"><span>Открытая мастерская художника Любови Мороз</span></h3>
          <div class="event-list__item-date">25 октября, 10:00</div>
        </li>
        <li class="event-list__item">
          <h3 class="event-list__item-title"><span>XOLIDAYBOY</span></h3>
          <div class="event-list__item-date">3 мар. 2027, 19:00</div>
        </li>
        <li class="event-list__item">
          <h3 class="event-list__item-title"><span>Гримуар Маркет</span></h3>
          <div class="event-list__item-date">24&thinsp;&ndash;&thinsp;25&nbsp;окт.</div>
        </li>
        <li class="event-list__item">
          <h3 class="event-list__item-title"><span>Ночной сеанс</span></h3>
          <div class="event-list__item-date event-multiple_long"
               title="28 окт., 8:30 и еще 4 сеанса">28 окт., 8:30 <span class="event-count">и еще 4 сеанса</span></div>
        </li>
        "#;
        let times = html_event_times(html);
        let time_of = |title: &str| times.get(&normalize_event_title(title)).map(String::as_str);
        assert_eq!(
            time_of("Открытая мастерская художника Любови Мороз"),
            Some("10:00")
        );
        assert_eq!(time_of("XOLIDAYBOY"), Some("19:00"));
        // Часы из одной цифры приводятся к двум, чтобы строки сравнивались.
        assert_eq!(time_of("Ночной сеанс"), Some("08:30"));
        // У события без времени пары нет — это не ошибка.
        assert_eq!(time_of("Гримуар Маркет"), None);
    }

    /// Карточка без блока с датой не должна получать время соседней карточки.
    #[test]
    fn event_without_date_does_not_steal_next_time() {
        let html = r#"
        <h3 class="event-list__item-title"><span>Без даты</span></h3>
        <a class="event-list__item-place">Площадка</a>
        <h3 class="event-list__item-title"><span>С датой</span></h3>
        <div class="event-list__item-date">25 октября, 10:00</div>
        "#;
        let times = html_event_times(html);
        assert_eq!(times.get(&normalize_event_title("Без даты")), None);
        assert_eq!(
            times
                .get(&normalize_event_title("С датой"))
                .map(String::as_str),
            Some("10:00")
        );
    }

    #[test]
    fn attaches_start_time_from_markup_to_json_ld_event() {
        let html = r#"
        <h3 class="event-list__item-title"><span>Семейный праздник «Новый годик с фондом &quot;Друг&quot;»</span></h3>
        <div class="event-list__item-date">25 октября, 10:00</div>
        <h3 class="event-list__item-title"><span>Гримуар Маркет</span></h3>
        <div class="event-list__item-date">24&thinsp;&ndash;&thinsp;25&nbsp;окт.</div>
        <script type="application/ld+json">
        {"@type":"Event","name":"Семейный праздник «Новый годик с фондом \"Друг\"»",
         "startDate":"2026-10-25","endDate":"2026-10-25","location":{"name":"Дом культуры"}}
        </script>
        <script type="application/ld+json">
        {"@type":"Event","name":"Гримуар Маркет","startDate":"2026-10-24","endDate":"2026-10-25"}
        </script>
        "#;
        let start = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let events = parse_events(html, start, end);
        assert_eq!(events.len(), 2);
        let morning = events
            .iter()
            .find(|event| event["title"].as_str().unwrap().starts_with("Семейный"))
            .unwrap();
        // Сопоставление по названию не зависит от регистра, кавычек и сущностей.
        assert_eq!(morning["start_time"], "10:00");
        let all_day = events
            .iter()
            .find(|event| event["title"] == "Гримуар Маркет")
            .unwrap();
        assert!(all_day.get("start_time").is_none());
    }

    /// Page 2 and friends carry the same markup, so their events get times too.
    #[test]
    fn reads_times_from_ajax_page_markup() {
        let html = r#"
        <li class="event-list__item">
          <h3 class="event-list__item-title"><span>Чайная церемония</span></h3>
          <div class="event-list__item-date">28 октября, 19:30</div>
        </li>
        "#;
        let times = html_event_times(html);
        assert_eq!(
            times
                .get(&normalize_event_title("Чайная церемония"))
                .map(String::as_str),
            Some("19:30")
        );
    }

    #[test]
    fn events_are_available_for_poster_cities() {
        assert!(events_url("Владивосток").is_some());
        assert!(events_url("Русский остров").is_some());
        assert!(events_url("Находка").is_some());
        assert!(events_url("Уссурийск").is_some());
        assert!(events_url("Большой Камень").is_some());
        assert!(events_url("Артём").is_some());
        assert!(events_url("Арсеньев").is_some());
    }
}
