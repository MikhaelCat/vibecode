use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::domain::dates::add_days;
use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::plan::parse_time;
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct WeatherTool;

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &'static str {
        "get_weather"
    }

    fn description(&self) -> &'static str {
        "Прогноз погоды по координатам и датам: осадки, ветер, температуры, восход и закат. Даты в формате YYYY-MM-DD."
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
            return Err(ToolError::BadArgs("end_date is before start_date".to_string()));
        }

        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
             &daily=temperature_2m_max,temperature_2m_min,precipitation_probability_max,\
             precipitation_sum,wind_speed_10m_max,sunrise,sunset,weather_code\
             &timezone=Asia%2FVladivostok&start_date={start}&end_date={end}"
        );

        if let Ok(response) = ctx.http.get(url).send().await {
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

        let days = mock_days(start, end);
        Ok(ToolOutput::ok(
            json!({"days": days, "forecast_available": false}),
            SourceKind::Mock,
            "Погода: демо-данные (провайдер недоступен)",
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
            .and_then(|time| parse_time(&time[..5.min(time.len())]));
        let sunset = text_at(daily, "sunset", index)
            .and_then(|value| value.split('T').nth(1))
            .and_then(|time| parse_time(&time[..5.min(time.len())]));

        days.push(json!({
            "date": date,
            "precip_prob": number_at(daily, "precipitation_probability_max", index).unwrap_or(0.0),
            "wind_max": number_at(daily, "wind_speed_10m_max", index).unwrap_or(0.0),
            "temp_min": number_at(daily, "temperature_2m_min", index).unwrap_or(0.0),
            "temp_max": number_at(daily, "temperature_2m_max", index).unwrap_or(0.0),
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

fn mock_days(start: NaiveDate, end: NaiveDate) -> Vec<Value> {
    let mut days = Vec::new();
    let mut date = start;
    let mut guard = 0;
    while date <= end && guard < 7 {
        days.push(json!({
            "date": date,
            "precip_prob": 10.0,
            "wind_max": 5.0,
            "temp_min": 8.0,
            "temp_max": 16.0,
            "sunrise": "07:30:00",
            "sunset": "18:30:00"
        }));
        date = add_days(date, 1);
        guard += 1;
    }
    days
}
