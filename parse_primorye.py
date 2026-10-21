#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Парсер данных для AI-ассистента путешествий по Приморскому краю (v2).

Главное отличие от v1: вместо одного тяжёлого запроса «по всему краю» к Overpass
делаются много лёгких запросов «вокруг точки» (around) — по городам и природным
опорным точкам. Они отвечают за секунды, как в вашем tools.py.

Создаёт JSON-файлы в --out (по умолчанию ./data):
    places.json         места и достопримечательности  (Overpass + Wikipedia)
    routes.json         маршруты между городами        (OSRM)
    weather.json        прогноз погоды                 (Open-Meteo)
    accommodation.json  жильё                          (Overpass)
    transport.json      вокзалы, станции, маршруты     (Overpass + Яндекс.Расписания, если есть ключ)
    fuel_stations.json  АЗС, GeoJSON (вокруг городов + вдоль трасс) (Overpass)

Зависимости:  pip install httpx
Запуск:       python parse_primorye.py
              python parse_primorye.py --only fuel,weather
"""

import argparse
import asyncio
import hashlib
import itertools
import json
import logging
import math
import os
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

import httpx

# --------------------------------------------------------------------------- #
# Настройки
# --------------------------------------------------------------------------- #

HEADERS = {"User-Agent": "PrimoryeTravelAssistant/1.0 (Hackathon T-Bank; contact: student@tbank.ru)"}
REGION = "Приморский край"
TZ = "Asia/Vladivostok"

OVERPASS_URLS = [
    "https://overpass.openstreetmap.ru/api/interpreter",  # российское зеркало, может быть быстрее из РФ
    "https://overpass.kumi.systems/api/interpreter",
    "https://overpass-api.de/api/interpreter",
    "https://overpass.private.coffee/api/interpreter",
    "https://maps.mail.ru/osm/tools/overpass/api/interpreter",  # зеркало VK, не проверено
    "https://overpass.openstreetmap.fr/api/interpreter",        # зеркало OSM-France, не проверено
]
OVERPASS_TASKS = ("places", "accommodation", "transport", "fuel")
PROBE_QUERY = "[out:json][timeout:10];node(43.1056,131.8735,43.1057,131.8736);out;"
OPEN_METEO_URL = "https://api.open-meteo.com/v1/forecast"
OSRM_BASE = {
    "car": "https://router.project-osrm.org/route/v1/driving",
    "bike": "https://routing.openstreetmap.de/routed-bike/route/v1/driving",
    "foot": "https://routing.openstreetmap.de/routed-foot/route/v1/driving",
}
YANDEX_RASP = "https://api.rasp.yandex.net/v3.0"
BBOX = "42.2,130.4,48.5,139.3"  # S,W,N,E — только для редких объектов (ж/д и паромные маршруты)

# Опорные точки. kind=city участвует в маршрутах; r — радиус поиска вокруг точки, км.
ANCHORS = [
    {"name": "Владивосток", "lat": 43.1056, "lon": 131.8735, "r": 25, "kind": "city"},
    {"name": "Остров Русский", "lat": 43.0350, "lon": 131.8000, "r": 15, "kind": "city"},
    {"name": "Уссурийск", "lat": 43.7973, "lon": 131.9518, "r": 25, "kind": "city"},
    {"name": "Находка", "lat": 42.8167, "lon": 132.8731, "r": 25, "kind": "city"},
    {"name": "Артём", "lat": 43.3550, "lon": 132.1880, "r": 20, "kind": "city"},
    {"name": "Большой Камень", "lat": 43.1136, "lon": 132.3481, "r": 20, "kind": "city"},
    {"name": "Партизанск", "lat": 43.1280, "lon": 133.1260, "r": 25, "kind": "city"},
    {"name": "Арсеньев", "lat": 44.1620, "lon": 133.2820, "r": 25, "kind": "city"},
    {"name": "Дальнегорск", "lat": 44.5540, "lon": 135.5700, "r": 30, "kind": "city"},
    {"name": "Лесозаводск", "lat": 45.4780, "lon": 133.4180, "r": 25, "kind": "city"},
    {"name": "Дальнереченск", "lat": 45.9300, "lon": 133.7330, "r": 25, "kind": "city"},
    {"name": "Спасск-Дальний", "lat": 44.5900, "lon": 132.8130, "r": 25, "kind": "city"},
    {"name": "Фокино", "lat": 42.9710, "lon": 132.4110, "r": 15, "kind": "city"},
    {"name": "Славянка", "lat": 42.8640, "lon": 131.3910, "r": 25, "kind": "city"},
    {"name": "Хасан", "lat": 42.4300, "lon": 130.6420, "r": 30, "kind": "city"},
    {"name": "Терней", "lat": 45.0500, "lon": 136.6000, "r": 40, "kind": "city"},
    # Природные районы вне городов (приблизительные центры, радиус покрывает неточность)
    {"name": "Озеро Ханка", "lat": 45.0000, "lon": 132.4000, "r": 40, "kind": "nature"},
    {"name": "Лазовский заповедник", "lat": 43.2500, "lon": 134.1000, "r": 40, "kind": "nature"},
    {"name": "Чугуевка (Сихотэ-Алинь)", "lat": 44.1700, "lon": 133.8800, "r": 40, "kind": "nature"},
    {"name": "Горнотаёжное", "lat": 43.6800, "lon": 132.1500, "r": 20, "kind": "nature"},
    {"name": "Кавалерово", "lat": 44.2700, "lon": 135.0700, "r": 40, "kind": "nature"},
]
CITIES = [a for a in ANCHORS if a["kind"] == "city"]
BY_NAME = {a["name"]: a for a in ANCHORS}

# Трассы, вдоль которых ищем АЗС (линия берётся из OSRM)
TRUNK_ROUTES = [
    ("Владивосток", "Уссурийск"), ("Уссурийск", "Спасск-Дальний"), ("Спасск-Дальний", "Лесозаводск"),
    ("Лесозаводск", "Дальнереченск"), ("Владивосток", "Находка"), ("Находка", "Партизанск"),
    ("Уссурийск", "Арсеньев"), ("Владивосток", "Большой Камень"), ("Артём", "Партизанск"),
]

WMO_RU = {
    0: "Ясно", 1: "Преимущественно ясно", 2: "Переменная облачность", 3: "Пасмурно",
    45: "Туман", 48: "Изморозь", 51: "Слабая морось", 53: "Морось", 55: "Сильная морось",
    56: "Ледяная морось", 57: "Сильная ледяная морось", 61: "Слабый дождь", 63: "Дождь",
    65: "Сильный дождь", 66: "Ледяной дождь", 67: "Сильный ледяной дождь",
    71: "Слабый снег", 73: "Снег", 75: "Сильный снег", 77: "Снежные зёрна",
    80: "Слабый ливень", 81: "Ливень", 82: "Сильный ливень",
    85: "Слабый снегопад", 86: "Сильный снегопад",
    95: "Гроза", 96: "Гроза с градом", 99: "Гроза с сильным градом",
}

log = logging.getLogger("parser")

# Глобальное состояние (создаётся в main)
CLIENT = None
SEM = None          # общий лимит одновременных запросов
OV_SEM = None       # лимит одновременных запросов к Overpass (он не любит параллельность)
OV_TIMEOUT = 60
CACHE_DIR = None
_ROUTE_MEMO = {}


# --------------------------------------------------------------------------- #
# Сетевые утилиты
# --------------------------------------------------------------------------- #

async def get_json(url, params=None, retries=3, timeout=60.0):
    last = None
    for i in range(retries):
        try:
            async with SEM:
                r = await CLIENT.get(url, params=params, timeout=timeout)
            if r.status_code in (429, 500, 502, 503, 504):
                raise httpx.HTTPError(f"HTTP {r.status_code}")
            r.raise_for_status()
            return r.json()
        except (httpx.HTTPError, ValueError) as e:
            last = e
            await asyncio.sleep(1.5 * 2 ** i)
    raise RuntimeError(f"GET {url} не удался: {last!r}")


def demote_mirror(url):
    """Мирор, который не ответил, уходит в конец очереди — следующие запросы не тратят на него время."""
    if url in OVERPASS_URLS and OVERPASS_URLS[-1] != url:
        OVERPASS_URLS.remove(url)
        OVERPASS_URLS.append(url)


async def overpass(body):
    """Лёгкий Overpass-запрос: кэш на диске, смена зеркала при ошибке."""
    query = f"[out:json][timeout:{OV_TIMEOUT}];{body}"
    cache_file = None
    if CACHE_DIR:
        cache_file = CACHE_DIR / (hashlib.sha1(query.encode("utf-8")).hexdigest() + ".json")
        if cache_file.exists():
            return json.loads(cache_file.read_text(encoding="utf-8"))

    last = None
    for url in list(OVERPASS_URLS):
        host = url.split("/")[2]
        t0 = time.time()
        try:
            async with OV_SEM:
                r = await CLIENT.post(url, data={"data": query},
                                      timeout=httpx.Timeout(OV_TIMEOUT + 15, connect=10.0))
            if r.status_code == 200:
                data = r.json()
                if data.get("elements") or "remark" not in data:
                    els = data.get("elements", [])
                    log.debug("Overpass %s: %d объектов за %.1f c", host, len(els), time.time() - t0)
                    if cache_file:
                        cache_file.parent.mkdir(parents=True, exist_ok=True)
                        cache_file.write_text(json.dumps(els, ensure_ascii=False), encoding="utf-8")
                    return els
                last = f"{host}: {data['remark'][:120]}"
            else:
                last = f"{host}: HTTP {r.status_code}"
        except (httpx.HTTPError, ValueError) as e:
            last = f"{host}: {e!r}"
        log.warning("Overpass: %s — пробую следующее зеркало", last)
        demote_mirror(url)
        await asyncio.sleep(2)
    raise RuntimeError(f"Overpass недоступен: {last}")


async def gather_progress(label, coros):
    """Выполняет корутины параллельно, пишет прогресс, ошибки одной не валят остальные (результат None)."""
    done = 0
    total = len(coros)

    async def wrap(c):
        nonlocal done
        try:
            res = await c
        except Exception as e:  # noqa: BLE001
            log.warning("%s: %s", label, e)
            res = None
        done += 1
        log.info("%s: %d/%d", label, done, total)
        return res

    return await asyncio.gather(*[wrap(c) for c in coros])


# --------------------------------------------------------------------------- #
# Общие функции
# --------------------------------------------------------------------------- #

def haversine_km(lat1, lon1, lat2, lon2):
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp, dl = p2 - p1, math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * 6371.0 * math.asin(math.sqrt(a))


def nearest_city(lat, lon, max_km=50):
    if lat is None or lon is None:
        return None
    best = min(ANCHORS, key=lambda c: haversine_km(lat, lon, c["lat"], c["lon"]))
    d = haversine_km(lat, lon, best["lat"], best["lon"])
    return {"name": best["name"], "distance_km": round(d, 1)} if d <= max_km else None


def el_coords(el):
    if el["type"] == "node":
        return el.get("lat"), el.get("lon")
    c = el.get("center")
    return (c["lat"], c["lon"]) if c else (None, None)


def el_id(el):
    return f"{el['type']}/{el['id']}"


def tag_address(t):
    if t.get("addr:full"):
        return t["addr:full"]
    parts = [t.get("addr:city"), t.get("addr:street"), t.get("addr:housenumber")]
    return ", ".join(p for p in parts if p) or None


def first_tag(t, *keys):
    for k in keys:
        if t.get(k):
            return t[k]
    return None


def around(a, radius_km=None):
    return f"(around:{int((radius_km or a['r']) * 1000)},{a['lat']},{a['lon']})"


def meta(source, count, **extra):
    m = {"region": REGION, "source": source, "count": count,
         "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds")}
    m.update(extra)
    return m


def write_json(path, obj):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(obj, f, ensure_ascii=False, indent=2)
    tmp.replace(path)
    log.info("Сохранён %s (%.1f КБ)", path, path.stat().st_size / 1024)


async def overpass_per_anchor(label, anchors, make_body):
    """Один лёгкий запрос на опорную точку. Возвращает (уникальные элементы, список неудавшихся точек)."""
    async def one(a):
        return await overpass(make_body(a))

    results = await gather_progress(label, [one(a) for a in anchors])
    failed = [a["name"] for a, r in zip(anchors, results) if r is None]
    if len(failed) == len(anchors):
        raise RuntimeError("все запросы к Overpass не удались — проверьте сеть/зеркала")
    unique = {}
    for els in results:
        for el in els or []:
            unique.setdefault(el_id(el), el)
    return list(unique.values()), failed


# --------------------------------------------------------------------------- #
# 1. Места
# --------------------------------------------------------------------------- #

def places_body(a):
    A = around(a)
    return (
        "("
        f'nwr["tourism"~"^(attraction|museum|viewpoint|gallery|zoo|aquarium|theme_park)$"]["name"]{A};'
        f'nwr["historic"~"^(monument|memorial|castle|fort|ruins|archaeological_site|battlefield|wreck)$"]["name"]{A};'
        f'nwr["natural"~"^(waterfall|cave_entrance|beach|hot_spring)$"]["name"]{A};'
        f'nwr["natural"~"^(peak|cape|bay|rock)$"]["name"]["wikidata"]{A};'
        f'nwr["man_made"="lighthouse"]["name"]{A};'
        f'nwr["leisure"="nature_reserve"]["name"]{A};'
        f'nwr["boundary"="national_park"]["name"]{A};'
        ");out tags center;"
    )


CATEGORY_KEYS = ("tourism", "historic", "natural", "man_made", "leisure", "boundary")


def place_category(t):
    for k in CATEGORY_KEYS:
        if t.get(k):
            return f"{k}={t[k]}"
    return "other"


async def enrich_wikipedia(items):
    """Краткое описание и миниатюра из Википедии по тегу wikipedia=lang:Title."""
    by_lang = {}
    for it in items:
        wp = it.get("wikipedia")
        if wp and ":" in wp:
            lang, title = wp.split(":", 1)
            by_lang.setdefault(lang, {}).setdefault(title, []).append(it)

    async def job(lang, titles, chunk):
        params = {
            "action": "query", "format": "json", "formatversion": 2, "redirects": 1,
            "prop": "extracts|pageimages", "exintro": 1, "explaintext": 1, "exlimit": 20,
            "piprop": "thumbnail", "pithumbsize": 600, "titles": "|".join(chunk),
        }
        pages, norm, cont = {}, {}, {}
        while True:
            data = await get_json(f"https://{lang}.wikipedia.org/w/api.php", {**params, **cont}, retries=2)
            q = data.get("query", {})
            for n in q.get("normalized", []) + q.get("redirects", []):
                norm[n["from"]] = n["to"]
            for p in q.get("pages", []):
                pages.setdefault(p.get("title"), {}).update({k: v for k, v in p.items() if v})
            if "continue" in data:
                cont = data["continue"]
            else:
                break
        for t in chunk:
            final = t
            for _ in range(3):
                final = norm.get(final, final)
            p = pages.get(final)
            if not p or p.get("missing"):
                continue
            extract = (p.get("extract") or "").strip()
            if len(extract) > 700:
                extract = extract[:700].rsplit(" ", 1)[0] + "…"
            thumb = (p.get("thumbnail") or {}).get("source")
            for it in titles[t]:
                if extract:
                    it["wikipedia_extract"] = extract
                if thumb:
                    it["image"] = it.get("image") or thumb
                it["wikipedia_url"] = f"https://{lang}.wikipedia.org/wiki/{final.replace(' ', '_')}"

    jobs = []
    for lang, titles in by_lang.items():
        keys = list(titles)
        for i in range(0, len(keys), 20):
            jobs.append(job(lang, titles, keys[i:i + 20]))
    if jobs:
        await gather_progress("Википедия", jobs)


async def parse_places(args):
    elements, failed = await overpass_per_anchor("Места", ANCHORS, places_body)
    items, seen = [], set()
    for el in elements:
        t = el.get("tags", {})
        lat, lon = el_coords(el)
        if lat is None or not t.get("name"):
            continue
        key = (t["name"], round(lat, 3), round(lon, 3))
        if key in seen:
            continue
        seen.add(key)
        items.append({
            "id": el_id(el), "name": t["name"], "name_en": t.get("name:en"),
            "category": place_category(t), "lat": lat, "lon": lon,
            "nearest_city": nearest_city(lat, lon),
            "description": first_tag(t, "description:ru", "description"),
            "opening_hours": t.get("opening_hours"), "fee": t.get("fee"),
            "website": first_tag(t, "website", "contact:website", "url"),
            "phone": first_tag(t, "phone", "contact:phone"),
            "address": tag_address(t),
            "wikipedia": t.get("wikipedia"), "wikidata": t.get("wikidata"), "image": t.get("image"),
        })
    if not args.skip_wiki:
        await enrich_wikipedia(items)
    items.sort(key=lambda x: (x["wikipedia"] is None, x["name"]))
    return "places.json", {
        "meta": meta("OpenStreetMap Overpass + Wikipedia", len(items), failed_anchors=failed),
        "items": items,
    }


# --------------------------------------------------------------------------- #
# 2. Маршруты
# --------------------------------------------------------------------------- #

async def osrm_route(a, b, profile="car", geometry=True):
    key = (a["name"], b["name"], profile, geometry)
    if key in _ROUTE_MEMO:
        return _ROUTE_MEMO[key]
    url = f"{OSRM_BASE[profile]}/{a['lon']},{a['lat']};{b['lon']},{b['lat']}"
    params = {"overview": "simplified" if geometry else "false", "geometries": "geojson", "steps": "false"}
    d = await get_json(url, params, retries=3)
    if d.get("code") != "Ok" or not d.get("routes"):
        raise RuntimeError(d.get("code", "маршрут не найден"))
    _ROUTE_MEMO[key] = d["routes"][0]
    await asyncio.sleep(0.4)  # публичный сервер просит не частить
    return _ROUTE_MEMO[key]


async def parse_routes(args):
    profiles = [p.strip() for p in args.profiles.split(",") if p.strip()]
    bad = [p for p in profiles if p not in OSRM_BASE]
    if bad:
        raise ValueError(f"неизвестные профили {bad}, доступны: {', '.join(OSRM_BASE)}")
    geometry = not args.no_geometry
    jobs, meta_jobs = [], []
    for profile in profiles:
        for a, b in itertools.combinations(CITIES, 2):
            meta_jobs.append((a, b, profile))
            jobs.append(osrm_route(a, b, profile, geometry))
    results = await gather_progress("Маршруты", jobs)

    items, failed = [], []
    for (a, b, profile), r in zip(meta_jobs, results):
        if r is None:
            failed.append({"from": a["name"], "to": b["name"], "profile": profile})
            continue
        item = {"from": a["name"], "to": b["name"], "profile": profile,
                "distance_km": round(r["distance"] / 1000, 1),
                "duration_min": round(r["duration"] / 60), "symmetric": True}
        if geometry:
            item["geometry"] = r["geometry"]
        items.append(item)
    return "routes.json", {
        "meta": meta("OSRM / routing.openstreetmap.de", len(items), profiles=profiles, failed=failed),
        "points": [{k: c[k] for k in ("name", "lat", "lon")} for c in CITIES],
        "items": items,
    }


# --------------------------------------------------------------------------- #
# 3. Погода
# --------------------------------------------------------------------------- #

async def parse_weather(args):
    days = max(1, min(args.weather_days, 16))
    daily_vars = ["weather_code", "temperature_2m_max", "temperature_2m_min", "precipitation_sum",
                  "precipitation_probability_max", "wind_speed_10m_max", "wind_gusts_10m_max",
                  "sunrise", "sunset"]

    async def one(c):
        d = await get_json(OPEN_METEO_URL, {
            "latitude": c["lat"], "longitude": c["lon"], "timezone": TZ, "forecast_days": days,
            "wind_speed_unit": "ms", "daily": ",".join(daily_vars),
            "current": "temperature_2m,precipitation,wind_speed_10m,weather_code",
        })
        dl = d["daily"]
        forecast = []
        for i, date in enumerate(dl["time"]):
            code = dl["weather_code"][i]
            forecast.append({
                "date": date, "weather_code": code, "summary": WMO_RU.get(code, "Нет данных"),
                "t_max": dl["temperature_2m_max"][i], "t_min": dl["temperature_2m_min"][i],
                "precipitation_mm": dl["precipitation_sum"][i],
                "precipitation_probability": dl["precipitation_probability_max"][i],
                "wind_max_ms": dl["wind_speed_10m_max"][i], "gusts_max_ms": dl["wind_gusts_10m_max"][i],
                "sunrise": dl["sunrise"][i], "sunset": dl["sunset"][i],
            })
        cur = d.get("current", {})
        return {
            "city": c["name"], "lat": c["lat"], "lon": c["lon"],
            "current": {"time": cur.get("time"), "temperature": cur.get("temperature_2m"),
                        "precipitation_mm": cur.get("precipitation"), "wind_ms": cur.get("wind_speed_10m"),
                        "summary": WMO_RU.get(cur.get("weather_code"), "Нет данных")},
            "forecast": forecast,
        }

    results = await gather_progress("Погода", [one(c) for c in ANCHORS])
    items = [r for r in results if r]
    if not items:
        raise RuntimeError("Open-Meteo не ответил ни для одной точки")
    return "weather.json", {
        "meta": meta("Open-Meteo", len(items), timezone=TZ,
                     units={"temperature": "°C", "wind": "м/с", "precipitation": "мм"}),
        "items": items,
    }


# --------------------------------------------------------------------------- #
# 4. Жильё
# --------------------------------------------------------------------------- #

def lodging_body(a):
    A = around(a)
    return (
        "("
        f'nwr["tourism"~"^(hotel|hostel|guest_house|motel|apartment|chalet|camp_site|caravan_site|alpine_hut|wilderness_hut)$"]["name"]{A};'
        f'nwr["leisure"="resort"]["name"]{A};'
        ");out tags center;"
    )


async def parse_accommodation(args):
    elements, failed = await overpass_per_anchor("Жильё", ANCHORS, lodging_body)
    items = []
    for el in elements:
        t = el.get("tags", {})
        lat, lon = el_coords(el)
        if lat is None or not t.get("name"):
            continue
        items.append({
            "id": el_id(el), "name": t["name"],
            "type": t.get("tourism") or ("resort" if t.get("leisure") == "resort" else "other"),
            "lat": lat, "lon": lon, "nearest_city": nearest_city(lat, lon),
            "stars": t.get("stars"), "rooms": t.get("rooms"), "address": tag_address(t),
            "phone": first_tag(t, "phone", "contact:phone"), "email": first_tag(t, "email", "contact:email"),
            "website": first_tag(t, "website", "contact:website", "url"),
            "opening_hours": t.get("opening_hours"), "internet_access": t.get("internet_access"),
            "fee": t.get("fee"), "price_note": first_tag(t, "charge", "price"), "wheelchair": t.get("wheelchair"),
        })
    items.sort(key=lambda x: ((x["nearest_city"] or {}).get("name", "я"), x["name"]))
    return "accommodation.json", {
        "meta": meta("OpenStreetMap Overpass", len(items), failed_anchors=failed,
                     note="Цен и свободных дат в OSM нет — добавьте мок или отдельный источник."),
        "items": items,
    }


# --------------------------------------------------------------------------- #
# 5. Транспорт
# --------------------------------------------------------------------------- #

def stations_body(a):
    A = around(a)
    return (
        "("
        f'nwr["amenity"="bus_station"]["name"]{A};'
        f'nwr["railway"~"^(station|halt)$"]["name"]{A};'
        f'nwr["amenity"="ferry_terminal"]["name"]{A};'
        f'nwr["aeroway"="aerodrome"]["name"]{A};'
        ");out tags center;"
    )


def station_kind(t):
    if t.get("amenity") == "bus_station":
        return "bus_station"
    if t.get("railway") in ("station", "halt"):
        return "train_" + t["railway"]
    if t.get("amenity") == "ferry_terminal":
        return "ferry_terminal"
    if t.get("aeroway") == "aerodrome":
        return "airport"
    return "other"


async def yandex_city_codes(key):
    log.info("Яндекс.Расписания: загрузка справочника станций (может занять минуту)…")
    data = await get_json(f"{YANDEX_RASP}/stations_list/",
                          {"apikey": key, "lang": "ru_RU", "format": "json"}, timeout=300.0)
    codes = {}
    for country in data.get("countries", []):
        for region in country.get("regions", []):
            if region.get("title") == REGION:
                for s in region.get("settlements", []):
                    code = (s.get("codes") or {}).get("yandex_code")
                    if code and s.get("title"):
                        codes[s["title"]] = code
    return codes


async def yandex_schedules(key, date, city_names):
    codes = await yandex_city_codes(key)
    chosen = [n for n in city_names if n in codes]
    for n in city_names:
        if n not in codes:
            log.warning("Нет кода Яндекса для: %s", n)

    async def one(a, b):
        d = await get_json(f"{YANDEX_RASP}/search/", {
            "apikey": key, "from": codes[a], "to": codes[b], "date": date, "lang": "ru_RU",
            "format": "json", "limit": 100, "transfers": "false"}, retries=2)
        out = []
        for seg in d.get("segments", []):
            th = seg.get("thread", {})
            out.append({
                "from_city": a, "to_city": b, "date": date,
                "from_station": (seg.get("from") or {}).get("title"),
                "to_station": (seg.get("to") or {}).get("title"),
                "departure": seg.get("departure"), "arrival": seg.get("arrival"),
                "duration_min": round(seg["duration"] / 60) if seg.get("duration") else None,
                "transport_type": th.get("transport_type"), "number": th.get("number"),
                "title": th.get("title"), "carrier": (th.get("carrier") or {}).get("title"),
                "stops": seg.get("stops"),
            })
        return out

    results = await gather_progress("Яндекс.Расписания",
                                    [one(a, b) for a, b in itertools.permutations(chosen, 2)])
    return [s for r in results if r for s in r]


async def parse_transport(args):
    elements, failed = await overpass_per_anchor("Транспорт: станции", ANCHORS, stations_body)
    stations = []
    for el in elements:
        t = el.get("tags", {})
        lat, lon = el_coords(el)
        if lat is None or not t.get("name"):
            continue
        stations.append({
            "id": el_id(el), "name": t["name"], "kind": station_kind(t), "lat": lat, "lon": lon,
            "nearest_city": nearest_city(lat, lon), "operator": t.get("operator"),
            "ref": first_tag(t, "ref", "railway:ref", "iata", "icao"),
            "opening_hours": t.get("opening_hours"),
            "website": first_tag(t, "website", "contact:website"),
            "phone": first_tag(t, "phone", "contact:phone"),
        })

    routes, notes = [], []
    try:  # маршруты ж/д, паромов и автобусов — редкие объекты, bbox-запрос быстрый
        rels = await overpass(f'relation["route"~"^(train|ferry|bus)$"]({BBOX});out tags center;')
        for el in rels:
            t = el.get("tags", {})
            lat, lon = el_coords(el)
            near = nearest_city(lat, lon, max_km=120)
            if near is None:  # отсекаем соседние регионы и Китай
                continue
            routes.append({
                "id": el_id(el), "type": t.get("route"), "ref": t.get("ref"), "name": t.get("name"),
                "from": t.get("from"), "to": t.get("to"), "operator": t.get("operator"),
                "network": t.get("network"), "interval": t.get("interval"),
                "opening_hours": t.get("opening_hours"), "nearest_city": near,
            })
    except Exception as e:  # noqa: BLE001
        notes.append(f"Маршруты общественного транспорта не получены: {e}")
        log.warning(notes[-1])

    schedules = []
    key = args.yandex_key or os.environ.get("YANDEX_RASP_KEY")
    if key:
        date = args.yandex_date or (datetime.now() + timedelta(days=1)).strftime("%Y-%m-%d")
        try:
            schedules = await yandex_schedules(key, date, [c.strip() for c in args.yandex_cities.split(",")])
        except Exception as e:  # noqa: BLE001
            notes.append(f"Яндекс.Расписания не получены: {e}")
            log.warning(notes[-1])
    else:
        notes.append("Ключ Яндекс.Расписаний не задан (YANDEX_RASP_KEY) — расписаний рейсов нет.")

    return "transport.json", {
        "meta": meta("OpenStreetMap Overpass + Яндекс.Расписания", len(stations), stations=len(stations),
                     routes=len(routes), schedules=len(schedules), failed_anchors=failed, notes=notes),
        "stations": stations, "routes": routes, "schedules": schedules,
    }


# --------------------------------------------------------------------------- #
# 6. Карта АЗС (GeoJSON): вокруг городов + вдоль трасс
# --------------------------------------------------------------------------- #

FUEL_TAG_NAMES = {
    "fuel:octane_92": "АИ-92", "fuel:octane_95": "АИ-95", "fuel:octane_98": "АИ-98",
    "fuel:octane_100": "АИ-100", "fuel:diesel": "ДТ", "fuel:lpg": "Газ (LPG)",
    "fuel:cng": "Метан (CNG)", "fuel:e85": "E85", "fuel:electricity": "Электро",
}


def thin(coords, max_points=60):
    """coords — [[lon, lat], ...] -> 'lat,lon,lat,lon,...' не более max_points точек."""
    step = max(1, math.ceil(len(coords) / max_points))
    pts = coords[::step]
    if pts[-1] != coords[-1]:
        pts.append(coords[-1])
    return ",".join(f"{lat:.5f},{lon:.5f}" for lon, lat in pts)


async def parse_fuel(args):
    # 1) вокруг городов и природных точек
    around_els, failed = await overpass_per_anchor(
        "АЗС: города", ANCHORS, lambda a: f'nwr["amenity"="fuel"]{around(a, max(a["r"], 30))};out tags center;')

    # 2) вдоль трасс: линия из OSRM, АЗС в 2.5 км от неё
    async def corridor(a_name, b_name):
        r = await osrm_route(BY_NAME[a_name], BY_NAME[b_name], "car", True)
        line = thin(r["geometry"]["coordinates"])
        els = await overpass(f'nwr["amenity"="fuel"](around:2500,{line});out tags center;')
        return f"{a_name}–{b_name}", els

    corr_results = await gather_progress("АЗС: трассы", [corridor(a, b) for a, b in TRUNK_ROUTES])
    corridor_failed = [f"{a}–{b}" for (a, b), r in zip(TRUNK_ROUTES, corr_results) if r is None]

    found = {}
    for el in around_els:
        found[el_id(el)] = (el, [])
    for res in corr_results:
        if not res:
            continue
        label, els = res
        for el in els:
            found.setdefault(el_id(el), (el, []))[1].append(label)

    features = []
    for el, corridors in found.values():
        t = el.get("tags", {})
        lat, lon = el_coords(el)
        if lat is None:
            continue
        fuels = [label for k, label in FUEL_TAG_NAMES.items() if t.get(k) == "yes"]
        features.append({
            "type": "Feature",
            "geometry": {"type": "Point", "coordinates": [lon, lat]},
            "properties": {
                "id": el_id(el), "name": first_tag(t, "name", "brand", "operator") or "АЗС",
                "brand": t.get("brand"), "operator": t.get("operator"), "address": tag_address(t),
                "opening_hours": t.get("opening_hours"), "fuels": fuels, "fuels_known": bool(fuels),
                "payment_cards": t.get("payment:cards"), "toilets": t.get("toilets"), "shop": t.get("shop"),
                "nearest_city": nearest_city(lat, lon, max_km=60), "on_corridors": corridors,
                "in_stock": None,  # наличие топлива в OSM нет — поле для мока/своего парсера
            },
        })
    return "fuel_stations.json", {
        "type": "FeatureCollection",
        "meta": meta("OpenStreetMap Overpass", len(features), failed_anchors=failed,
                     failed_corridors=corridor_failed,
                     note="Наличие топлива в реальном времени отсутствует (in_stock = null)."),
        "features": features,
    }


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #

TASKS = {
    "places": parse_places, "routes": parse_routes, "weather": parse_weather,
    "accommodation": parse_accommodation, "transport": parse_transport, "fuel": parse_fuel,
}


async def probe_overpass(verbose=False):
    """Параллельно пингует все зеркала Overpass, ставит живые в начало очереди. True, если хоть одно ответило."""
    async def ping(url):
        t0 = time.time()
        try:
            r = await CLIENT.post(url, data={"data": PROBE_QUERY}, timeout=httpx.Timeout(25.0, connect=10.0))
            return url, r.status_code == 200, f"HTTP {r.status_code}", time.time() - t0
        except httpx.HTTPError as e:
            return url, False, repr(e), time.time() - t0

    res = await asyncio.gather(*[ping(u) for u in list(OVERPASS_URLS)])
    alive = sorted([r for r in res if r[1]], key=lambda r: r[3])
    dead = [r for r in res if not r[1]]
    OVERPASS_URLS[:] = [r[0] for r in alive] + [r[0] for r in dead]
    if verbose:
        print("Проверка зеркал Overpass (крошечный запрос):")
        for url, ok, info, dt in res:
            print(f"  {'OK    ' if ok else 'НЕТ   '} {url}\n         {info}, {dt:.1f} c")
    else:
        for url, ok, info, dt in alive:
            log.info("Overpass жив: %s (%.1f c)", url.split("/")[2], dt)
    return bool(alive)


async def amain(args):
    global CLIENT, SEM, OV_SEM, OV_TIMEOUT, CACHE_DIR, OVERPASS_URLS
    out_dir = Path(args.out)
    OV_TIMEOUT = args.overpass_timeout
    CACHE_DIR = None if args.no_cache else out_dir / ".cache"
    if args.overpass_url:
        OVERPASS_URLS = [args.overpass_url]
    SEM = asyncio.Semaphore(6)
    OV_SEM = asyncio.Semaphore(args.overpass_concurrency)

    selected = [s.strip() for s in args.only.split(",") if s.strip()]
    results = {}
    async with httpx.AsyncClient(headers=HEADERS, follow_redirects=True,
                                 timeout=httpx.Timeout(60.0, connect=10.0)) as client:
        CLIENT = client
        if args.check_overpass:
            await probe_overpass(verbose=True)
            return 0
        overpass_ok = True
        if any(t in selected for t in OVERPASS_TASKS) and not args.skip_probe:
            log.info("Проверяю зеркала Overpass (до 25 c)…")
            overpass_ok = await probe_overpass()
            if not overpass_ok:
                log.error("Ни одно зеркало Overpass не отвечает — задачи на Overpass пропускаю "
                          "(запустите --check-overpass позже или с другой сети; --skip-probe чтобы всё равно пробовать)")
        for name in selected:
            if name in OVERPASS_TASKS and not overpass_ok:
                results[name] = "ПРОПУЩЕНО: Overpass недоступен"
                continue
            t0 = time.time()
            try:
                filename, payload = await TASKS[name](args)
                write_json(out_dir / filename, payload)
                results[name] = "OK"
            except Exception as e:  # noqa: BLE001
                log.error("%s: ошибка — %s", name, e)
                results[name] = f"ОШИБКА: {e}"
            log.info("%s: %.1f c", name, time.time() - t0)

    print("\nИтог:")
    for k, v in results.items():
        print(f"  {k:14s} {v}")
    return 0 if all(v == "OK" for v in results.values()) else 1


def main():
    ap = argparse.ArgumentParser(description="Парсер данных для ассистента по Приморскому краю")
    ap.add_argument("--out", default="data", help="папка для JSON (по умолчанию ./data)")
    ap.add_argument("--only", default=",".join(TASKS), help=f"что парсить через запятую: {','.join(TASKS)}")
    ap.add_argument("--skip-wiki", action="store_true", help="не обогащать места из Википедии")
    ap.add_argument("--weather-days", type=int, default=7, help="дней прогноза, 1–16 (по умолчанию 7)")
    ap.add_argument("--profiles", default="car", help="профили маршрутов: car,bike,foot (по умолчанию car)")
    ap.add_argument("--no-geometry", action="store_true", help="маршруты без геометрии линии")
    ap.add_argument("--yandex-key", default=None, help="ключ API Яндекс.Расписаний (или env YANDEX_RASP_KEY)")
    ap.add_argument("--yandex-date", default=None, help="дата расписания YYYY-MM-DD (по умолчанию завтра)")
    ap.add_argument("--yandex-cities", default="Владивосток,Уссурийск,Находка,Артём,Арсеньев,Партизанск",
                    help="города для расписаний (каждая пара = 1 запрос к API)")
    ap.add_argument("--overpass-timeout", type=int, default=60, help="секунд на один запрос Overpass")
    ap.add_argument("--overpass-concurrency", type=int, default=2, help="параллельных запросов к Overpass")
    ap.add_argument("--overpass-url", default=None, help="использовать только это зеркало Overpass")
    ap.add_argument("--no-cache", action="store_true", help="не брать ответы Overpass из кэша")
    ap.add_argument("--skip-probe", action="store_true", help="не проверять зеркала Overpass перед запуском")
    ap.add_argument("--check-overpass", action="store_true", help="только проверить, какие зеркала Overpass отвечают")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.INFO,
                        format="%(asctime)s %(levelname)s %(message)s", datefmt="%H:%M:%S")
    logging.getLogger("httpx").setLevel(logging.WARNING)

    unknown = [s for s in (x.strip() for x in args.only.split(",")) if s and s not in TASKS]
    if unknown:
        ap.error(f"неизвестные задачи: {', '.join(unknown)}")
    sys.exit(asyncio.run(amain(args)))


if __name__ == "__main__":
    main()
