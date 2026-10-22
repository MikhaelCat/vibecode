use std::time::Duration;

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::plan::parse_time;
use crate::tools::providers::geo;
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct WeatherTool;

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &'static str {
        "get_weather"
    }

    fn description(&self) -> &'static str {
        "Прогноз погоды по координатам и датам: осадки, ветер, температуры, восход и закат. Вызывай для района каждого дня поездки, а не только для стартового города."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "lat": {"type": "number"},
                "lon": {"type": "number"},
                "start_date": {"type": "string"},
                "end_date": {"type": "string"}
            },
            "required": ["lat", "lon", "start_date", "end_date"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let lat = args
            .get("lat")
            .and_then(Value::as_f64)
            .ok_or_else(|| ToolError::BadArgs("lat is required".to_string()))?;
        let lon = args
            .get("lon")
            .and_then(Value::as_f64)
            .ok_or_else(|| ToolError::BadArgs("lon is required".to_string()))?;
        let start = args
            .get("start_date")
            .and_then(Value::as_str)
            .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
            .unwrap_or(ctx.intent.start_date);
        let end = args
            .get("end_date")
            .and_then(Value::as_str)
            .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
            .unwrap_or(ctx.intent.end_date);
        if end < start {
            return Err(ToolError::BadArgs(
                "end_date is before start_date".to_string(),
            ));
        }

        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
             &daily=temperature_2m_max,temperature_2m_min,precipitation_probability_max,\
             precipitation_sum,wind_speed_10m_max,sunrise,sunset,weather_code\
             &timezone=Asia%2FVladivostok&start_date={start}&end_date={end}"
        );

        if let Ok(Ok(response)) =
            tokio::time::timeout(Duration::from_secs(3), ctx.http.get(url).send()).await
        {
            if response.status().is_success() {
                if let Ok(value) = response.json::<Value>().await {
                    let days = parse_days(&value);
                    if !days.is_empty() {
                        return Ok(ToolOutput::ok(
                            json!({"days": days, "forecast_available": true}),
                            SourceKind::Live,
                            "Прогноз Open-Meteo",
                        ));
                    }
                }
            }
        }

        // Open-Meteo can be unreachable from some networks. VL.ru publishes a
        // 9-day forecast for Primorye cities as Next.js JSON; use the nearest
        // city as a clearly-labelled live fallback.
        if let Some((city, slug)) = geo::nearest_city(lat, lon) {
            if let Some(days) = vlru_days(ctx, slug, start, end).await {
                if !days.is_empty() {
                    return Ok(ToolOutput::ok(
                        json!({"days": days, "forecast_available": true}),
                        SourceKind::Live,
                        format!("VL.ru: погода по {city}"),
                    ));
                }
            }
        }

        // Never invent weather: an unavailable forecast is reported as missing
        // data, and the validator marks those days as "not confirmed".
        Ok(ToolOutput::ok(
            json!({"days": [], "forecast_available": false}),
            SourceKind::Stale,
            "Погода: прогноз недоступен",
        ))
    }
}

fn parse_days(value: &Value) -> Vec<Value> {
    let Some(daily) = value.get("daily") else {
        return Vec::new();
    };
    let Some(times) = daily.get("time").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut days = Vec::new();
    for (index, time) in times.iter().enumerate() {
        let Some(date_text) = time.as_str() else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
            continue;
        };
        let sunrise = text_at(daily, "sunrise", index)
            .and_then(|value| value.split('T').nth(1))
            .and_then(|time| time.get(..5).and_then(parse_time));
        let sunset = text_at(daily, "sunset", index)
            .and_then(|value| value.split('T').nth(1))
            .and_then(|time| time.get(..5).and_then(parse_time));
        let (Some(temp_max), Some(temp_min), Some(precip_prob)) = (
            number_at(daily, "temperature_2m_max", index),
            number_at(daily, "temperature_2m_min", index),
            number_at(daily, "precipitation_probability_max", index),
        ) else {
            // A missing temperature is not 0 °C and a missing probability is
            // not a dry day. Drop the day so the caller can fall back.
            continue;
        };
        let wind_max = number_at(daily, "wind_speed_10m_max", index).unwrap_or(0.0);

        days.push(json!({
            "date": date,
            "precip_prob": precip_prob,
            "wind_max": wind_max,
            "temp_min": temp_min,
            "temp_max": temp_max,
            "sunrise": sunrise.map(|time| time.format("%H:%M:%S").to_string()),
            "sunset": sunset.map(|time| time.format("%H:%M:%S").to_string())
        }));
    }
    days
}

fn number_at(value: &Value, key: &str, index: usize) -> Option<f64> {
    value
        .get(key)
        .and_then(Value::as_array)
        .and_then(|array| array.get(index))
        .and_then(Value::as_f64)
}

fn text_at<'a>(value: &'a Value, key: &str, index: usize) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_array)
        .and_then(|array| array.get(index))
        .and_then(Value::as_str)
}

async fn vlru_days(
    ctx: &ToolCtx,
    slug: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Option<Vec<Value>> {
    let url = format!("https://www.vl.ru/weather/{slug}");
    let response = tokio::time::timeout(
        Duration::from_secs(4),
        ctx.http
            .get(url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (compatible; vibecode-travel-ai/0.1; +https://github.com/MikhaelCat/vibecode)",
            )
            .send(),
    )
    .await
    .ok()?
    .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let html = response.text().await.ok()?;
    let json_text = next_data(&html)?;
    let value: Value = serde_json::from_str(json_text).ok()?;
    let days = parse_vlru_days(&value, start, end);
    if days.is_empty() {
        None
    } else {
        Some(days)
    }
}

fn next_data(html: &str) -> Option<&str> {
    let marker = "id=\"__NEXT_DATA__\"";
    let start = html.find(marker)?;
    let rest = &html[start..];
    let tag_end = rest.find('>')?;
    let rest = &rest[tag_end + 1..];
    let end = rest.find("</script>")?;
    Some(&rest[..end])
}

fn parse_vlru_days(value: &Value, start: NaiveDate, end: NaiveDate) -> Vec<Value> {
    let Some(days) = value
        .get("props")
        .and_then(|props| props.get("pageProps"))
        .and_then(|props| props.get("forecasts"))
        .and_then(|forecasts| forecasts.get("days"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for day in days {
        let Some(date_text) = day.get("date").and_then(Value::as_str) else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
            continue;
        };
        if date < start || date > end {
            continue;
        }
        let description = day.get("description").and_then(Value::as_str).unwrap_or("");
        let (Some(temp_min), Some(temp_max)) = (
            day.get("temperature_low").and_then(Value::as_f64),
            day.get("temperature_high").and_then(Value::as_f64),
        ) else {
            continue;
        };
        let precipitation = day
            .get("precipitation")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let description_lower = description.to_lowercase();
        let precip_prob = if description_lower.contains("дождь")
            || description_lower.contains("ливень")
            || description_lower.contains("снег")
            || precipitation >= 5.0
        {
            80.0
        } else if precipitation >= 1.0 {
            60.0
        } else if precipitation > 0.0 {
            40.0
        } else {
            0.0
        };
        result.push(json!({
            "date": date,
            "precip_prob": precip_prob,
            "wind_max": day.get("wind_speed").and_then(Value::as_f64).unwrap_or(0.0),
            "temp_min": temp_min,
            "temp_max": temp_max,
            "sunrise": null,
            "sunset": null,
            "description": description
        }));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vlru_days_and_filters_dates() {
        let value = json!({
            "props": {"pageProps": {"forecasts": {"days": [
                {"date": "2026-10-24", "temperature_high": 12, "temperature_low": 5,
                 "wind_speed": 7, "precipitation": 1.2, "description": "Небольшой дождь"},
                {"date": "2026-10-25", "temperature_high": 10, "temperature_low": 3,
                 "wind_speed": 5, "precipitation": 0, "description": "Безоблачно"},
                {"date": "2026-10-30", "temperature_high": 8, "temperature_low": 2,
                 "wind_speed": 4, "precipitation": 0, "description": "Облачно"}
            ]}}}
        });
        let start = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let days = parse_vlru_days(&value, start, end);
        assert_eq!(days.len(), 2);
        assert_eq!(days[0]["temp_max"].as_f64(), Some(12.0));
        assert_eq!(days[0]["precip_prob"].as_f64(), Some(80.0));
        assert_eq!(days[1]["precip_prob"].as_f64(), Some(0.0));
    }
}
