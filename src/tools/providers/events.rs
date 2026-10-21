//! VL.ru events parser.
//!
//! The city poster exposes schema.org `Event` objects as JSON-LD, so this
//! provider parses structured data instead of scraping markup. Events are
//! filtered to the trip window, deduplicated by title/date/place and returned
//! with `live` provenance; upstream failures surface as `ToolError::Upstream`.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

const EVENTS_URL: &str = "https://www.vl.ru/afisha/vladivostok";
const EVENTS_USER_AGENT: &str =
    "Mozilla/5.0 (compatible; vibecode-travel-ai/0.1; +https://github.com/MikhaelCat/vibecode)";

/// Live events from the VL.ru city poster. The page exposes schema.org Event
/// objects as JSON-LD, so we parse structured data instead of scraping markup.
pub struct EventsTool;

#[async_trait]
impl Tool for EventsTool {
    fn name(&self) -> &'static str {
        "find_events"
    }

    fn description(&self) -> &'static str {
        "Живые события и мероприятия Владивостока из афиши VL.ru на даты поездки. Возвращает id, название, дату, место и ссылку."
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
        let start = parse_date_arg(&args, "start_date").unwrap_or(ctx.intent.start_date);
        let end = parse_date_arg(&args, "end_date").unwrap_or(ctx.intent.end_date);
        if end < start {
            return Err(ToolError::BadArgs(
                "end_date не может быть раньше start_date".to_string(),
            ));
        }

        let response = ctx
            .http
            .get(EVENTS_URL)
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
        let html = response
            .text()
            .await
            .map_err(|error| ToolError::Upstream(format!("VL.ru: {error}")))?;

        let mut events = parse_events(&html, start, end);
        events.truncate(12);
        let summary = if events.is_empty() {
            format!("VL.ru: событий на {} — {} не найдено", start, end)
        } else {
            format!("VL.ru: {} событий на даты поездки", events.len())
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
fn parse_events(html: &str, start: NaiveDate, end: NaiveDate) -> Vec<Value> {
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
            events.push(json!({
                "id": event.id,
                "title": event.title,
                "date": event_start,
                "end_date": event_end,
                "place": event.place,
                "url": event.url,
                "source": "live"
            }));
        }
    }
    events.sort_by(|a, b| {
        let left = a.get("date").and_then(Value::as_str).unwrap_or("");
        let right = b.get("date").and_then(Value::as_str).unwrap_or("");
        left.cmp(right)
    });
    events
}

/// Intermediate event record parsed from one schema.org object.
struct ParsedEvent {
    id: String,
    title: String,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    place: String,
    url: Option<String>,
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
    let url = value.get("url").and_then(Value::as_str).map(str::to_string);
    let id = url.clone().unwrap_or_else(|| {
        format!(
            "vl:event:{}:{}",
            start.map(|d| d.to_string()).unwrap_or_default(),
            title
        )
    });
    Some(ParsedEvent {
        id,
        title,
        start,
        end,
        place,
        url,
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
}
