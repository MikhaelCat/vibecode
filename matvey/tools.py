"""
Модуль инструментов (tools.py) для AI-ассистента путешествий по Приморскому краю.
Проект: Т-Банк x Сириус (Хакатон)

Содержит:
1. Асинхронные функции вызова внешних бесплатных API (Open-Meteo, Nominatim, Wikipedia, OSRM, Overpass).
2. JSON-схему TOOLS_SCHEMA для передачи в OpenRouter (параметр `tools`).
3. Маппинг функций TOOL_MAP для автоматического исполнения агентом.
"""

import httpx
from typing import Dict, Any, List, Optional

# Уникальный User-Agent обязателен для OpenStreetMap Nominatim и Wikipedia
HEADERS = {
    "User-Agent": "PrimoryeTravelAssistant/1.0 (Hackathon T-Bank; contact: student@tbank.ru)"
}


async def get_coordinates(place_name: str) -> Dict[str, Any]:
    """
    Геокодинг: получение географических координат по названию места в Приморском крае.
    Использует бесплатный сервис OpenStreetMap Nominatim.
    """
    query = f"{place_name}, Приморский край, Россия"
    url = "https://nominatim.openstreetmap.org/search"
    params = {
        "q": query,
        "format": "json",
        "limit": 1
    }
    
    try:
        async with httpx.AsyncClient(timeout=10.0, headers=HEADERS) as client:
            res = await client.get(url, params=params)
            res.raise_for_status()
            data = res.json()
            
            if not data:
                # Попробуем без явного указания региона, если запрос уже содержит уточнение
                fallback_res = await client.get(url, params={"q": place_name, "format": "json", "limit": 1})
                fallback_data = fallback_res.json()
                if not fallback_data:
                    return {"status": "error", "message": f"Место '{place_name}' не найдено на карте."}
                data = fallback_data

            return {
                "status": "success",
                "name": data[0].get("display_name"),
                "latitude": float(data[0]["lat"]),
                "longitude": float(data[0]["lon"])
            }
    except Exception as e:
        return {"status": "error", "message": f"Ошибка геокодинга: {str(e)}"}


async def get_weather_forecast(latitude: float, longitude: float, days: int = 2) -> Dict[str, Any]:
    """
    Прогноз погоды на указанное количество дней (от 1 до 7) через Open-Meteo.
    100% бесплатно, не требует API-ключа.
    """
    days = max(1, min(days, 7))
    url = "https://api.open-meteo.com/v1/forecast"
    params = {
        "latitude": latitude,
        "longitude": longitude,
        "forecast_days": days,
        "daily": "temperature_2m_max,temperature_2m_min,precipitation_sum,windspeed_10m_max",
        "timezone": "Asia/Vladivostok"
    }
    
    try:
        async with httpx.AsyncClient(timeout=10.0) as client:
            res = await client.get(url, params=params)
            res.raise_for_status()
            data = res.json()
            daily = data.get("daily", {})
            
            forecasts = []
            dates = daily.get("time", [])
            for i in range(len(dates)):
                precip = daily["precipitation_sum"][i]
                wind = daily["windspeed_10m_max"][i]
                forecasts.append({
                    "date": dates[i],
                    "temp_max_celsius": daily["temperature_2m_max"][i],
                    "temp_min_celsius": daily["temperature_2m_min"][i],
                    "precipitation_mm": precip,
                    "wind_speed_kmh": wind,
                    "conditions_summary": "Дождь/осадки" if precip > 1.0 else ("Ветрено" if wind > 30 else "Благоприятно")
                })
                
            return {
                "status": "success",
                "latitude": latitude,
                "longitude": longitude,
                "timezone": "Asia/Vladivostok",
                "forecast": forecasts
            }
    except Exception as e:
        return {"status": "error", "message": f"Ошибка получения погоды: {str(e)}"}


async def get_nearby_attractions(latitude: float, longitude: float, radius_meters: int = 15000) -> Dict[str, Any]:
    """
    Поиск реальных культурных и природных достопримечательностей через Wikipedia API Geosearch.
    Помогает избежать галлюцинаций модели о несуществующих объектах.
    """
    url = "https://ru.wikipedia.org/w/api.php"
    params = {
        "action": "query",
        "list": "geosearch",
        "gscoord": f"{latitude}|{longitude}",
        "gsradius": radius_meters,
        "gslimit": 8,
        "format": "json"
    }
    
    try:
        async with httpx.AsyncClient(timeout=10.0, headers=HEADERS) as client:
            res = await client.get(url, params=params)
            res.raise_for_status()
            data = res.json()
            items = data.get("query", {}).get("geosearch", [])
            
            attractions = [
                {
                    "title": item["title"],
                    "distance_meters": round(item["dist"]),
                    "latitude": item["lat"],
                    "longitude": item["lon"],
                    "wiki_page_id": item["pageid"]
                }
                for item in items
            ]
            return {"status": "success", "count": len(attractions), "attractions": attractions}
    except Exception as e:
        return {"status": "error", "message": f"Ошибка поиска достопримечательностей: {str(e)}"}


async def calculate_route(start_lat: float, start_lon: float, end_lat: float, end_lon: float, mode: str = "driving") -> Dict[str, Any]:
    """
    Расчет маршрута, расстояния и времени в пути через публичный сервер OSRM.
    mode: 'driving' (автомобиль) или 'walking' (пешком).
    Внимание: OSRM ожидает координаты в формате lon,lat.
    """
    valid_mode = "walking" if mode == "walking" else "driving"
    coordinates = f"{start_lon},{start_lat};{end_lon},{end_lat}"
    url = f"http://router.project-osrm.org/route/v1/{valid_mode}/{coordinates}"
    params = {
        "overview": "false",
        "steps": "false"
    }
    
    try:
        async with httpx.AsyncClient(timeout=10.0) as client:
            res = await client.get(url, params=params)
            res.raise_for_status()
            data = res.json()
            
            if data.get("code") != "Ok" or not data.get("routes"):
                return {"status": "error", "message": "Маршрут не найден или точки недостижимы по суше."}
                
            route = data["routes"][0]
            distance_km = round(route["distance"] / 1000, 1)
            duration_min = round(route["duration"] / 60, 1)
            duration_hours = round(duration_min / 60, 1)
            
            return {
                "status": "success",
                "mode": valid_mode,
                "distance_km": distance_km,
                "duration_minutes": duration_min,
                "duration_hours": duration_hours
            }
    except Exception as e:
        return {"status": "error", "message": f"Ошибка расчета маршрута: {str(e)}"}


async def get_nearby_gas_stations(latitude: float, longitude: float, radius_meters: int = 15000) -> Dict[str, Any]:
    """
    Поиск АЗС вдоль маршрута через Overpass API (OpenStreetMap).
    Критично для автопутешествий по Приморью.
    """
    url = "https://overpass-api.de/api/interpreter"
    query = f"""
    [out:json][timeout:10];
    node["amenity"="fuel"](around:{radius_meters},{latitude},{longitude});
    out body 5;
    """
    
    try:
        async with httpx.AsyncClient(timeout=12.0, headers=HEADERS) as client:
            res = await client.post(url, data={"data": query})
            res.raise_for_status()
            data = res.json()
            elements = data.get("elements", [])
            
            stations = []
            for el in elements:
                tags = el.get("tags", {})
                stations.append({
                    "name": tags.get("name") or tags.get("operator") or "АЗС",
                    "brand": tags.get("brand"),
                    "latitude": el.get("lat"),
                    "longitude": el.get("lon")
                })
                
            return {
                "status": "success",
                "count": len(stations),
                "stations": stations
            }
    except Exception as e:
        # Fallback при таймауте Overpass (он может быть нагружен)
        return {
            "status": "warning",
            "message": "Данные по АЗС временно недоступны из Overpass API.",
            "stations": []
        }


# ============================================================================
# JSON-схема инструментов для передачи в OpenRouter / OpenAI API
# ============================================================================

TOOLS_SCHEMA: List[Dict[str, Any]] = [
    {
        "type": "function",
        "function": {
            "name": "get_coordinates",
            "description": "Определяет точные географические координаты (широту и долготу) населенного пункта, сопки, бухты или острова в Приморском крае.",
            "parameters": {
                "type": "object",
                "properties": {
                    "place_name": {
                        "type": "string",
                        "description": "Название места в Приморском крае (например, 'Владивосток', 'Находка', 'Ливадия', 'мыс Тобизина', 'остров Русский')"
                    }
                },
                "required": ["place_name"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "get_weather_forecast",
            "description": "Возвращает реальный прогноз погоды (температура, осадки в мм, порывы ветра) для заданных координат в Приморье.",
            "parameters": {
                "type": "object",
                "properties": {
                    "latitude": { "type": "number", "description": "Широта (например 43.1155)" },
                    "longitude": { "type": "number", "description": "Долгота (например 131.8855)" },
                    "days": { "type": "integer", "description": "Количество дней прогноза (от 1 до 7)", "default": 2 }
                },
                "required": ["latitude", "longitude"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "get_nearby_attractions",
            "description": "Ищет реальные подтвержденные достопримечательности, природные объекты и музеи вокруг указанных координат.",
            "parameters": {
                "type": "object",
                "properties": {
                    "latitude": { "type": "number", "description": "Широта центра поиска" },
                    "longitude": { "type": "number", "description": "Долгота центра поиска" },
                    "radius_meters": { "type": "integer", "description": "Радиус поиска в метрах", "default": 15000 }
                },
                "required": ["latitude", "longitude"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "calculate_route",
            "description": "Рассчитывает расстояние (км) и время в пути между двумя географическими точками на автомобиле или пешком.",
            "parameters": {
                "type": "object",
                "properties": {
                    "start_lat": { "type": "number", "description": "Широта точки отправления" },
                    "start_lon": { "type": "number", "description": "Долгота точки отправления" },
                    "end_lat": { "type": "number", "description": "Широта точки назначения" },
                    "end_lon": { "type": "number", "description": "Долгота точки назначения" },
                    "mode": { "type": "string", "enum": ["driving", "walking"], "default": "driving", "description": "Способ передвижения" }
                },
                "required": ["start_lat", "start_lon", "end_lat", "end_lon"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "get_nearby_gas_stations",
            "description": "Проверяет наличие АЗС (заправок) вокруг указанных координат для автомобильного маршрута.",
            "parameters": {
                "type": "object",
                "properties": {
                    "latitude": { "type": "number", "description": "Широта точки на маршруте" },
                    "longitude": { "type": "number", "description": "Долгота точки на маршруте" },
                    "radius_meters": { "type": "integer", "description": "Радиус поиска в метрах", "default": 15000 }
                },
                "required": ["latitude", "longitude"]
            }
        }
    }
]

# Карта диспетчеризации для выполнения функций в агентном цикле
TOOL_MAP = {
    "get_coordinates": get_coordinates,
    "get_weather_forecast": get_weather_forecast,
    "get_nearby_attractions": get_nearby_attractions,
    "calculate_route": calculate_route,
    "get_nearby_gas_stations": get_nearby_gas_stations
}