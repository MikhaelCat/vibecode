/* vibecode frontend — vanilla JS SPA, no build step.
   Reads the SSE stream from POST /api/plan (same-origin only), renders days,
   lodging and a lazily created Leaflet map.

   Point guarantees:
   - every plan item is rendered in the day list, with or without coordinates;
   - items without coordinates get an explicit note instead of a pin;
   - Leaflet is loaded with CDN fallbacks (index.html). The map is built as
     soon as the library arrives — even after a plan was rendered — and if it
     never arrives the points are listed inside the map card instead of
     silently disappearing;
   - markers of all days stay on the map at once, and the view fits them. */
const API = '';
const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

const MAX_DAYS = 7;
// Must match PRIMORYE_BBOX in src/domain/geo.rs. Also the home bounds of the
// map: the default pannable box, extended only if a plan point leaves it.
const PRIMORYE_BBOX = { south: 42.2, west: 130.0, north: 48.5, east: 139.1 };
const MAP_MAX_BOUNDS = [[41.5, 128.5], [49.5, 141.0]];
// One accent colour per day: [line, pin] so markers of different days stay
// distinguishable while the whole plan is visible at once.
const DAY_PALETTE = [
  ['#14b8a6', '#0f766e'],
  ['#3b82f6', '#1d4ed8'],
  ['#f97316', '#c2410c'],
  ['#8b5cf6', '#6d28d9'],
  ['#ef4444', '#b91c1c'],
  ['#06b6d4', '#0e7490'],
  ['#eab308', '#a16207'],
];
const EXAMPLES = [
  'Хочу на выходные по Приморью на машине, не больше двух дней',
  'Один день у моря возле Владивостока, без долгих переездов',
  'Три дня: маяки, бухты и хорошая погода',
  'Что посмотреть на острове Русский с детьми?',
];
const STAGES = {
  guard: 'Проверяю тему',
  intent: 'Разбираю запрос',
  fallback: 'Упрощаю план',
  plan: 'Собираю план',
  validation: 'Проверяю план',
  done: 'Готово',
};
const transportRu = { car: 'машина', foot: 'пешком', bike: 'велосипед', transit: 'автобус/поезд' };
const kindRu = {
  sight: 'достопримечательность',
  museum: 'музей',
  nature: 'природа',
  viewpoint: 'смотровая',
  park: 'парк',
  historic: 'история',
  beach: 'пляж',
  lake: 'озеро',
  river: 'река',
  cinema: 'кино',
  theater: 'театр',
  market: 'рынок',
  shop: 'магазин',
  food: 'еда',
  bar: 'бар',
  club: 'клуб',
  transfer: 'переезд',
  rest: 'отдых',
};
const lodgingKindRu = { hotel: 'отель', guesthouse: 'гостевой дом', apartment: 'апартаменты', hostel: 'хостел' };
const MONTHS = ['янв', 'фев', 'мар', 'апр', 'мая', 'июн', 'июл', 'авг', 'сен', 'окт', 'ноя', 'дек'];

// An item without coordinates arrives as (0,0) (unknown place) or with missing
// fields. It must stay in the day list and only skip the map pin. The backend
// already rejects water-body stops and points outside Primorye, so the
// frontend pins every finite, non-zero coordinate instead of filtering names.
function itemPoint(stop) {
  if (!stop || typeof stop !== 'object') return null;
  const rawLat = stop.lat ?? stop.latitude;
  const rawLon = stop.lon ?? stop.lng ?? stop.longitude;
  if (rawLat === undefined || rawLat === null || rawLat === ''
    || rawLon === undefined || rawLon === null || rawLon === '') return null;
  const lat = Number(rawLat);
  const lon = Number(rawLon);
  if (!Number.isFinite(lat) || !Number.isFinite(lon)) return null;
  if (lat < -90 || lat > 90 || lon < -180 || lon > 180) return null;
  if (Math.abs(lat) < 0.0001 && Math.abs(lon) < 0.0001) return null;
  return [lat, lon];
}

// ---------- карта ----------
let map = null;
let layer = null;
let markersByDay = [];
let mapTilesLoaded = false;
let mapRendered = false;
let fitPending = false;
let mapFallbackTimer = 0;
// index.html loads Leaflet with CDN fallbacks and reports the outcome both as
// events and as window.__leafletState, so a listener registered after the
// library (or its failure) already arrived still sees the result.
let leafletState = window.L ? 'ready' : (window.__leafletState || 'loading');

window.addEventListener('leaflet-ready', () => {
  leafletState = 'ready';
  ensureMap();
});
window.addEventListener('leaflet-unavailable', () => {
  leafletState = 'failed';
  if (plan && !map) showMapFallback('Карта недоступна: библиотека Leaflet не загрузилась. Точки маршрута показаны списком.');
});

function ensureMap() {
  if (map) return true;
  if (!window.L) return false;
  return initMap();
}

function initMap() {
  if (map || !window.L) return false;
  const container = $('map');
  if (!container) return false;
  // One-finger page scrolling stays native only on devices without any fine
  // pointer; hybrid laptops keep mouse drag and double-click zoom.
  const coarseOnly = window.matchMedia('(any-pointer: coarse)').matches
    && !window.matchMedia('(any-pointer: fine)').matches;
  map = L.map('map', {
    zoomControl: true,
    attributionControl: false,
    // Wheel-zoom guard: the wheel zooms the map only after the user clicks it,
    // otherwise the page keeps scrolling normally over the map card.
    scrollWheelZoom: false,
    wheelDebounceTime: 40,
    wheelPxPerZoomLevel: 90,
    dragging: !coarseOnly,
    touchZoom: !coarseOnly,
    doubleClickZoom: !coarseOnly,
    boxZoom: false,
    keyboard: false,
    tapHold: false,
    preferCanvas: true,
    minZoom: 6,
    maxBounds: MAP_MAX_BOUNDS,
    maxBoundsViscosity: 1.0,
  }).setView([43.7, 133.5], 8);
  const el = map.getContainer();
  if (coarseOnly) el.style.touchAction = 'pan-y';
  map.on('click', () => map.scrollWheelZoom.enable());
  el.addEventListener('mouseleave', () => map.scrollWheelZoom.disable());

  addTileLayer(map);
  layer = L.layerGroup().addTo(map);
  map.whenReady(() => map.invalidateSize());
  setTimeout(() => {
    if (mapTilesLoaded) return;
    const loading = $('map-loading');
    if (!loading) return;
    loading.textContent = 'Подложка карты не загрузилась — точки маршрута нанесены на схему';
    loading.classList.remove('hidden');
  }, 6000);
  onMapReady();
  return true;
}

// Basemap: CARTO Voyager first (light, matches the UI), OpenStreetMap as the
// fallback when CARTO is unreachable. Both are attribution-safe tiles.
function addTileLayer(target) {
  const common = { maxZoom: 18, keepBuffer: 1, updateWhenIdle: true, detectRetina: false, noWrap: true };
  const sources = [
    { url: 'https://{s}.basemaps.cartocdn.com/rastertiles/voyager/{z}/{x}/{y}.png', options: { ...common, subdomains: 'abcd' } },
    { url: 'https://tile.openstreetmap.org/{z}/{x}/{y}.png', options: { ...common, bounds: L.latLngBounds(MAP_MAX_BOUNDS) } },
  ];
  let index = 0;
  let errors = 0;
  const markLoaded = () => {
    if (mapTilesLoaded) return;
    mapTilesLoaded = true;
    const loading = $('map-loading');
    if (loading) loading.classList.add('hidden');
  };
  const attach = () => {
    const tiles = L.tileLayer(sources[index].url, sources[index].options).addTo(target);
    tiles.on('tileload', markLoaded);
    tiles.on('tileerror', () => {
      errors += 1;
      if (mapTilesLoaded || errors < 4 || index >= sources.length - 1) return;
      target.removeLayer(tiles);
      index += 1;
      errors = 0;
      attach();
    });
  };
  attach();
}

function onMapReady() {
  clearTimeout(mapFallbackTimer);
  const box = $('map-fallback');
  if (box) box.hidden = true;
  const loading = $('map-loading');
  if (loading && !mapTilesLoaded) {
    loading.textContent = 'Загружаю карту…';
    loading.classList.remove('hidden');
  }
  mapRendered = false;
  if (plan) renderMap();
  // Tab clicks and stop clicks from the day list need the map to exist.
  if (map) map.invalidateSize();
}

// When Leaflet never loads, the points must still be visible: list them inside
// the map card, coordinates included, instead of leaving an empty canvas.
function showMapFallback(message) {
  const box = $('map-fallback');
  const loading = $('map-loading');
  if (loading) loading.classList.add('hidden');
  if (!box) return;
  const days = plan ? (plan.days || []) : [];
  const html = days.map((dayPlan, dayIndex) => {
    const items = planItems(dayPlan);
    const rows = items.map((stop, itemIndex) => {
      const point = itemPoint(stop);
      const coords = point
        ? `${point[0].toFixed(3)}, ${point[1].toFixed(3)}`
        : 'координаты не определены';
      return `<li><span>${itemIndex + 1}</span><b>${esc(stop.name)}</b><i>${esc(coords)}</i></li>`;
    }).join('');
    return `<section><h3>День ${dayIndex + 1} · ${esc(fmtDate(dayPlan.date))}</h3>
      <ol>${rows || '<li><span>—</span><b>Свободный день</b><i></i></li>'}</ol></section>`;
  }).join('');
  box.innerHTML = (message ? `<p class="map-fallback-note">${esc(message)}</p>` : '') + html;
  box.hidden = false;
}

function scheduleMapFallback() {
  clearTimeout(mapFallbackTimer);
  if (map || !plan) return;
  mapFallbackTimer = setTimeout(() => {
    if (map || !plan) return;
    showMapFallback('Карта не загрузилась: библиотека Leaflet недоступна. Точки маршрута показаны списком.');
  }, 10000);
}

function pinIcon(light, dark, label) {
  return L.divIcon({
    className: 'pin-wrap',
    html: `<div class="pin" style="--pin:${light};--pin-dark:${dark}"><span>${label}</span></div>`,
    iconSize: [34, 42],
    iconAnchor: [17, 40],
    popupAnchor: [0, -38],
    tooltipAnchor: [0, -36],
  });
}

function dayBounds(points) {
  const bounds = L.latLngBounds(points[0], points[0]);
  if (points.length === 1) {
    bounds.extend([points[0][0] + 0.05, points[0][1] + 0.05]);
    bounds.extend([points[0][0] - 0.05, points[0][1] - 0.05]);
  } else {
    points.forEach((point) => bounds.extend(point));
  }
  return bounds;
}

// Auto-fit to every point of the whole plan, not only the active day. The
// pannable box grows only when a point leaves the default Primorye window.
function fitPlanBounds(points) {
  if (!map || !points.length) return;
  const target = dayBounds(points).pad(0.18);
  const home = L.latLngBounds(MAP_MAX_BOUNDS);
  const panned = target.pad(0.4).extend(home.getSouthWest()).extend(home.getNorthEast());
  map.setMaxBounds(panned);
  map.invalidateSize({ pan: false });
  map.fitBounds(target, { maxZoom: 13, animate: false });
}

// Marker + route rendering for the entire plan. Runs on every new plan and
// again whenever the map becomes ready, so a late Leaflet never eats points.
function renderMap() {
  if (!map || !layer || !plan) return;
  layer.clearLayers();
  markersByDay = [];
  const allPoints = [];
  (plan.days || []).forEach((dayPlan, dayIndex) => {
    const items = planItems(dayPlan);
    const [line, pin] = DAY_PALETTE[dayIndex % DAY_PALETTE.length];
    const dayMarkers = [];
    items.forEach((stop, itemIndex) => {
      const point = itemPoint(stop);
      if (!point) return;
      allPoints.push(point);
      const time = fmtTime(stop.start || stop.time);
      const end = fmtTime(stop.end);
      const range = time && end ? `${time}–${end}` : time;
      const marker = L.marker(point, {
        icon: pinIcon(line, pin, itemIndex + 1),
        title: stop.name || '',
      });
      marker.bindPopup(`<b>${esc(stop.name)}</b>${range ? `<br><span>День ${dayIndex + 1} · ${esc(range)}</span>` : `<br><span>День ${dayIndex + 1}</span>`}`);
      marker.bindTooltip(`День ${dayIndex + 1} · ${itemIndex + 1}. ${esc(stop.name || '')}`, {
        direction: 'top',
        offset: [0, -36],
      });
      marker.addTo(layer);
      dayMarkers[itemIndex] = marker;
    });
    markersByDay[dayIndex] = dayMarkers;
    drawDayRoute(dayPlan, line);
  });
  mapRendered = true;
  if (fitPending) {
    fitPending = false;
    fitPlanBounds(allPoints);
  }
}

// Route line of one day. Real leg geometry wins; otherwise the two pinned
// endpoints of the leg are connected with a dashed line (an approximation,
// never presented as a road route). Items without coordinates are skipped.
function drawDayRoute(dayPlan, color) {
  const items = planItems(dayPlan);
  const legs = Array.isArray(dayPlan.legs) ? dayPlan.legs : [];
  const pinned = [];
  items.forEach((stop, index) => {
    const point = itemPoint(stop);
    if (point) pinned.push({ index, point });
  });
  for (let i = 0; i + 1 < pinned.length; i += 1) {
    const from = pinned[i];
    const to = pinned[i + 1];
    const decoded = decodeGeometry(legs[from.index] && legs[from.index].geometry);
    const path = decoded || [from.point, to.point];
    const exact = Boolean(decoded);
    L.polyline(path, {
      color,
      weight: exact ? 4 : 3,
      opacity: exact ? 0.8 : 0.55,
      dashArray: exact ? null : '5 7',
      lineJoin: 'round',
      lineCap: 'round',
      interactive: false,
    }).addTo(layer);
  }
}

// Leg geometry may be a GeoJSON LineString (string or object), an array of
// [lat, lon] / [lon, lat] pairs, or an encoded polyline. Unusable values are
// rejected and the caller falls back to a dashed straight segment.
function decodeGeometry(value) {
  if (!value) return null;
  if (Array.isArray(value) || typeof value === 'object') return geometryToPath(value);
  if (typeof value !== 'string') return null;
  const text = value.trim();
  if (!text) return null;
  if (text[0] === '{' || text[0] === '[') {
    try {
      return geometryToPath(JSON.parse(text));
    } catch (err) {
      return null;
    }
  }
  return decodeEncodedPolyline(text);
}

function geometryToPath(data) {
  if (!data || typeof data !== 'object') return null;
  if (data.type === 'LineString') return pointsToPath(data.coordinates);
  if (data.type === 'MultiLineString') {
    const path = [];
    (data.coordinates || []).forEach((line) => (line || []).forEach((pair) => {
      const point = pairToLatLng(pair);
      if (point) path.push(point);
    }));
    return path.length > 1 ? path : null;
  }
  if (Array.isArray(data)) return pointsToPath(data);
  return null;
}

function pointsToPath(list) {
  const path = [];
  (list || []).forEach((entry) => {
    const point = pairToLatLng(entry);
    if (point) path.push(point);
  });
  return path.length > 1 ? path : null;
}

function pairToLatLng(entry) {
  if (entry && typeof entry === 'object' && !Array.isArray(entry)) {
    const lat = Number(entry.lat ?? entry.latitude);
    const lon = Number(entry.lon ?? entry.lng ?? entry.longitude);
    return Number.isFinite(lat) && Number.isFinite(lon) ? [lat, lon] : null;
  }
  if (!Array.isArray(entry) || entry.length < 2) return null;
  const first = Number(entry[0]);
  const second = Number(entry[1]);
  if (!Number.isFinite(first) || !Number.isFinite(second)) return null;
  // GeoJSON uses [lon, lat]; plain pairs in this project are [lat, lon].
  const latShaped = first >= PRIMORYE_BBOX.south - 5 && first <= PRIMORYE_BBOX.north + 5;
  return latShaped ? [first, second] : [second, first];
}

function decodeEncodedPolyline(encoded) {
  if (!/^[ -~]+$/.test(encoded)) return null;
  const path = [];
  let index = 0;
  let lat = 0;
  let lon = 0;
  while (index < encoded.length) {
    const latStep = polylineStep(encoded, index);
    if (!latStep) return null;
    index = latStep.index;
    lat += latStep.value;
    const lonStep = polylineStep(encoded, index);
    if (!lonStep) return null;
    index = lonStep.index;
    lon += lonStep.value;
    path.push([lat / 1e5, lon / 1e5]);
  }
  if (path.length < 2) return null;
  // A decoded polyline that leaves Primorye is not a route of this plan.
  const sane = path.every(([latValue, lonValue]) => latValue >= PRIMORYE_BBOX.south - 3
    && latValue <= PRIMORYE_BBOX.north + 3
    && lonValue >= PRIMORYE_BBOX.west - 3
    && lonValue <= PRIMORYE_BBOX.east + 3);
  return sane ? path : null;
}

function polylineStep(text, start) {
  let index = start;
  let shift = 0;
  let result = 0;
  let byte = 0;
  do {
    if (index >= text.length) return null;
    byte = text.charCodeAt(index) - 63;
    index += 1;
    result |= (byte & 0x1f) << shift;
    shift += 5;
  } while (byte >= 0x20);
  return { value: result & 1 ? ~(result >> 1) : result >> 1, index };
}

// Do not download tiles or create Leaflet layers until the map is near the
// viewport. A plan rendered earlier still draws itself via ensureMap().
const mapCard = document.querySelector('.map-card');
if (mapCard && 'IntersectionObserver' in window) {
  const observer = new IntersectionObserver((entries) => {
    if (entries.some((entry) => entry.isIntersecting)) {
      ensureMap();
      if (map) observer.disconnect();
    }
  }, { rootMargin: '500px' });
  observer.observe(mapCard);
}

// ---------- состояние ----------
let plan = null;
let day = 0;
let autoStart = '';
let autoEnd = '';
let running = false;
let sawDone = false;
const stageMap = new Map();

function isoDate(date) {
  const pad = (n) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}
function addDays(date, days) {
  const next = new Date(date);
  next.setDate(next.getDate() + days);
  return next;
}
function fmtDate(iso) {
  if (!iso) return '';
  const date = new Date(`${iso}T00:00:00`);
  if (Number.isNaN(date.getTime())) return String(iso);
  return `${date.getDate()} ${MONTHS[date.getMonth()]}`;
}
function fmtRange(start, end) {
  if (!start && !end) return '';
  if (!end || start === end) return fmtDate(start);
  return `${fmtDate(start)} — ${fmtDate(end)}`;
}
function fmtTime(value) {
  return value ? String(value).slice(0, 5) : '';
}
function dayTitle(title, firstStop) {
  const fallback = 'Маршрут дня';
  if (!title) return fallback;
  const stripped = title.replace(/^День\s+\d+\s*[—–-]\s*/i, '').trim();
  if (!stripped) return fallback;
  if (firstStop && stripped.toLowerCase() === String(firstStop).toLowerCase()) return fallback;
  return stripped;
}

function defaultDates() {
  const start = addDays(new Date(), 1);
  const end = addDays(start, 2);
  autoStart = isoDate(start);
  autoEnd = isoDate(end);
  $('start_date').value = autoStart;
  $('end_date').value = autoEnd;
  $('start_date').min = isoDate(new Date());
  syncDateBounds();
}

function datesAreCustom() {
  return $('start_date').value !== autoStart || $('end_date').value !== autoEnd;
}

function syncDateBounds() {
  const start = $('start_date').value;
  const end = $('end_date').value;
  if (!start) return;
  const startDate = new Date(`${start}T00:00:00`);
  const maxEnd = addDays(startDate, MAX_DAYS - 1);
  $('end_date').min = start;
  $('end_date').max = isoDate(maxEnd);
  if (end && new Date(`${end}T00:00:00`) < startDate) $('end_date').value = start;
  if (end && new Date(`${end}T00:00:00`) > maxEnd) $('end_date').value = isoDate(maxEnd);
}

function clampDates() {
  syncDateBounds();
  updateDateHint();
}

function updateDateHint() {
  const hint = document.querySelector('.hint');
  if (!hint) return;
  const dates = datesAreCustom()
    ? 'Даты из формы главнее текста запроса.'
    : 'Даты не тронуты — возьмём их из текста запроса.';
  const transport = $('transport').value
    ? 'Транспорт выбран в форме.'
    : 'Транспорт не выбран — по умолчанию автомобиль, если в тексте нет другого.';
  hint.textContent = `${dates} ${transport}`;
}

defaultDates();
$('start_date').addEventListener('change', clampDates);
$('end_date').addEventListener('change', clampDates);
$('transport').addEventListener('change', updateDateHint);

// ---------- старт ----------
$('chips').innerHTML = EXAMPLES.map((t) => `<button type="button" class="chip">${esc(t)}</button>`).join('');
$('chips').addEventListener('click', (e) => {
  const chip = e.target.closest('.chip');
  if (!chip) return;
  $('q').value = chip.textContent;
  updateCounter();
  $('q').focus();
});

$('q').addEventListener('input', updateCounter);
function updateCounter() {
  $('count').textContent = String($('q').value.length);
}
updateCounter();

$('token').value = localStorage.getItem('app_token') || '';
$('form').addEventListener('submit', (e) => {
  e.preventDefault();
  requestPlan($('q').value.trim());
});
$('retry').addEventListener('click', () => requestPlan($('q').value.trim()));

// ?query=... — prefill the field only. Sending always requires an explicit click.
const params = new URLSearchParams(location.search);
const prefill = params.get('query');
if (prefill) {
  $('q').value = prefill.slice(0, 1000);
  updateCounter();
}

fetch('/api/health')
  .then((r) => r.json())
  .then((h) => {
    const el = $('health');
    el.textContent = 'онлайн';
    el.classList.add('ok');
    if (h.auth_required) $('token-box').hidden = false;
  })
  .catch(() => {
    const el = $('health');
    el.textContent = 'нет связи';
    el.classList.add('bad');
  });

// ---------- запуск ----------
async function requestPlan(query) {
  if (!query || running) return;
  running = true;
  localStorage.setItem('app_token', $('token').value);
  reset();
  setBusy(true);

  try {
    const headers = { 'content-type': 'application/json' };
    if ($('token').value) headers['x-app-token'] = $('token').value;
    const payload = { query };
    if ($('transport').value) payload.transport = $('transport').value;
    if (datesAreCustom()) {
      payload.start_date = $('start_date').value || null;
      payload.end_date = $('end_date').value || null;
    }
    const res = await fetch('/api/plan', { method: 'POST', headers, body: JSON.stringify(payload) });
    if (!res.ok) {
      let message = `Сервер ответил ${res.status}.`;
      try {
        const body = await res.json();
        if (body && body.message) message = body.message;
        if (res.status === 401) $('token-box').hidden = false;
      } catch { /* keep status message */ }
      throw new Error(message);
    }
    await readSSE(res.body, handle);
    if (!sawDone && !plan && $('refusal').hidden) {
      showError('Соединение с сервером прервалось до готового плана. Попробуйте ещё раз.');
    }
  } catch (err) {
    console.warn(err);
    showError(err && err.message ? err.message : 'Проверьте соединение с сервером и попробуйте ещё раз.');
  } finally {
    running = false;
    setBusy(false);
  }
}

function setBusy(busy) {
  const button = $('go');
  button.disabled = busy;
  button.classList.toggle('loading', busy);
  const label = button.querySelector('.btn-label');
  if (label) label.textContent = busy ? 'Собираю…' : 'Собрать маршрут';
}

async function readSSE(body, cb) {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true }).replace(/\r/g, '');
    let index;
    while ((index = buffer.indexOf('\n\n')) >= 0) {
      const block = buffer.slice(0, index);
      buffer = buffer.slice(index + 2);
      let event = 'message';
      const data = [];
      for (const line of block.split('\n')) {
        if (line.startsWith('event:')) event = line.slice(6).trim();
        else if (line.startsWith('data:')) data.push(line.slice(5).trimStart());
      }
      const raw = data.join('\n');
      if (!raw) continue;
      try { cb(event, JSON.parse(raw)); }
      catch { cb(event, raw); }
    }
  }
}

// ---------- события ----------
function handle(event, data) {
  if (event === 'stage') {
    const label = STAGES[data.stage] || data.label || data.name || data.stage;
    if (label) stage(label, data.status === 'running' ? 'live' : 'done');
  }
  if (event === 'intent') {
    stage(STAGES.intent, 'done');
    if (!datesAreCustom() && data.start_date && data.end_date) {
      autoStart = data.start_date;
      autoEnd = data.end_date;
      $('start_date').value = autoStart;
      $('end_date').value = autoEnd;
      syncDateBounds();
      updateDateHint();
    }
  }
  if (event === 'plan') {
    plan = data.plan || data;
    day = 0;
    fitPending = true;
    mapRendered = false;
    stage(STAGES.plan, 'done');
    renderPlan();
  }
  if (event === 'validation') stage(STAGES.validation, 'done');
  if (event === 'refusal') {
    sawDone = true;
    stage(STAGES.done, 'done');
    refuse(data);
  }
  if (event === 'done') {
    sawDone = true;
    stage(STAGES.done, 'done');
  }
}

// ---------- UI ----------
function reset() {
  $('stages').innerHTML = '';
  $('refusal').hidden = true;
  $('refusal').innerHTML = '';
  $('error').hidden = true;
  $('results').hidden = true;
  $('result').hidden = true;
  $('lodging-box').hidden = true;
  $('lodging').innerHTML = '';
  $('events-box').hidden = true;
  $('events').innerHTML = '';
  $('stops').innerHTML = '';
  $('tabs').innerHTML = '';
  $('day-name').textContent = '';
  $('day-facts').innerHTML = '';
  $('day-kicker').textContent = 'День 1';
  $('map-caption').textContent = '';
  $('map-caption').hidden = true;
  stageMap.clear();
  if (layer) layer.clearLayers();
  markersByDay = [];
  mapRendered = false;
  fitPending = false;
  clearTimeout(mapFallbackTimer);
  const fallback = $('map-fallback');
  if (fallback) {
    fallback.hidden = true;
    fallback.innerHTML = '';
  }
  if (map) map.closePopup();
  plan = null;
  day = 0;
  sawDone = false;
}

function stage(text, state = 'live') {
  const list = $('stages');
  const existing = stageMap.get(text);
  if (existing) {
    if (state === 'done') existing.className = 'done';
    return;
  }
  list.querySelectorAll('.live').forEach((item) => { item.className = 'done'; });
  const item = document.createElement('li');
  item.className = state;
  item.innerHTML = `<span>${esc(text)}</span>`;
  list.appendChild(item);
  stageMap.set(text, item);
}

function refuse(data) {
  const box = $('refusal');
  const isTimeout = data && data.category === 'timeout';
  const heading = isTimeout ? 'Не удалось собрать план.' : 'Запрос не по теме.';
  const fallback = isTimeout
    ? 'Попробуйте более короткий запрос или повторите попытку.'
    : 'Я планирую только поездки по Приморскому краю.';
  box.hidden = false;
  box.innerHTML = `<div><b>${heading}</b> ${esc(data.message || data.reason || fallback)}</div>
    <button type="button" class="btn btn-small" id="refusal-example">Попробовать пример</button>`;
  const example = box.querySelector('#refusal-example');
  if (example) example.addEventListener('click', () => {
    $('q').value = EXAMPLES[0];
    updateCounter();
    requestPlan(EXAMPLES[0]);
  });
}

function showError(message) {
  $('results').hidden = true;
  $('result').hidden = true;
  $('error').hidden = false;
  $('error-text').textContent = message;
}

function planItems(dayPlan) {
  return dayPlan.items || dayPlan.stops || [];
}

function legText(leg) {
  if (!leg) return '';
  const mode = transportRu[leg.mode] || leg.mode || 'в пути';
  return `${mode} · ${(+leg.distance_km || 0).toFixed(1)} км · ${leg.duration_min || 0} мин`;
}

function renderPlan() {
  if (!plan) return;
  $('results').hidden = false;
  $('result').hidden = false;
  $('p-title').textContent = plan.title || 'Ваш маршрут';
  $('p-sum').textContent = plan.summary || '';

  const intent = plan.intent || {};
  const meta = [];
  if (intent.start_date) {
    meta.push(`<span class="pill pill-date">${esc(fmtRange(intent.start_date, intent.end_date))}</span>`);
  }
  const transport = plan.transport || intent.transport;
  if (transport) meta.push(`<span class="pill">${esc(transportRu[transport] || transport)}</span>`);
  if (plan.total_km) meta.push(`<span class="pill">${(+plan.total_km).toFixed(0)} км</span>`);
  if (plan.cost_estimate_rub) {
    meta.push(`<span class="pill">≈ ${Number(plan.cost_estimate_rub).toLocaleString('ru-RU')} ₽</span>`);
  }
  for (const weather of (plan.weather || []).slice(0, 3)) {
    meta.push(`<span class="pill pill-weather">${esc(fmtDate(weather.date))} · ${esc(weather.t_min ?? '')}…${esc(weather.t_max ?? '')}° · ${esc(weather.desc || '')}</span>`);
  }
  const warnings = [...new Set((plan.warnings || []).map((w) => w.message || w))];
  for (const warning of warnings) meta.push(`<span class="pill pill-warn">${esc(warning)}</span>`);
  $('p-meta').innerHTML = meta.join('');

  const days = plan.days || [];
  if (day >= days.length) day = 0;
  $('tabs').innerHTML = days.map((item, index) =>
    `<button class="tab" role="tab" data-i="${index}" aria-selected="${index === day}">
      <span>День ${index + 1}</span><small>${esc(fmtDate(item.date))}</small>
    </button>`).join('');
  ensureMap();
  if (!map) {
    if (leafletState === 'failed') {
      showMapFallback('Карта недоступна: библиотека Leaflet не загрузилась. Точки маршрута показаны списком.');
    } else {
      scheduleMapFallback();
    }
  }
  renderDay();
  if (map && !mapRendered) renderMap();
  renderLodging();
}

$('tabs').addEventListener('click', (event) => {
  const button = event.target.closest('.tab');
  if (!button) return;
  day = Number(button.dataset.i);
  renderPlan();
});

function renderDay() {
  const current = (plan.days || [])[day];
  if (!current) return;
  const items = planItems(current);
  const legs = current.legs || [];

  $('day-kicker').textContent = `День ${day + 1} · ${fmtDate(current.date)}`;
  $('day-name').textContent = dayTitle(current.title, items[0]?.name);

  const facts = [];
  if (items.length) facts.push(`${items.length} ${items.length === 1 ? 'точка' : items.length < 5 ? 'точки' : 'точек'}`);
  const distance = legs.reduce((sum, leg) => sum + (+leg.distance_km || 0), 0);
  if (distance) facts.push(`${distance.toFixed(0)} км`);
  const minutes = legs.reduce((sum, leg) => sum + (+leg.duration_min || 0), 0);
  if (minutes) facts.push(`${Math.round(minutes / 60 * 10) / 10} ч в пути`);
  const weather = (plan.weather || []).find((w) => String(w.date) === String(current.date));
  if (weather) facts.push(`${weather.t_min}…${weather.t_max}° · ${weather.desc || ''}`);
  $('day-facts').innerHTML = facts.map((fact) => `<span>${esc(fact)}</span>`).join('');

  let html = '';
  items.forEach((stop, index) => {
    if (index > 0 && legs[index - 1]) html += legHtml(legs[index - 1]);
    html += stopHtml(stop, index);
  });
  if (!html) {
    html = `<li class="empty-day"><b>Свободный день.</b> Ассистент не нашёл проверенных точек на эту дату — можно оставить день для отдыха или поменять запрос.</li>`;
  }
  $('stops').innerHTML = html;
  renderEvents(current.events || []);
}

function stopDescription(stop) {
  if (stop.note) return stop.note;
  const parts = [];
  const kind = kindRu[stop.kind] || 'точка маршрута';
  parts.push(kind.charAt(0).toUpperCase() + kind.slice(1));
  if (stop.opening_hours) parts.push(`часы: ${stop.opening_hours}`);
  if (stop.price_rub) parts.push(`${stop.price_rub} ₽`);
  return parts.join(' · ');
}

function stopHtml(stop, index) {
  const time = fmtTime(stop.start || stop.time);
  const end = fmtTime(stop.end);
  const range = time && end ? `${time}–${end}` : time;
  const kind = kindRu[stop.kind] || '';
  const description = stopDescription(stop);
  const pinned = itemPoint(stop) !== null;
  return `<li class="stop" data-i="${index}" data-place-id="${esc(stop.place_id || '')}">
    <div class="stop-rail"><span class="stop-num">${String(index + 1).padStart(2, '0')}</span></div>
    <div class="stop-body">
      <div class="stop-top">
        ${range ? `<span class="stop-time">${esc(range)}</span>` : ''}
        ${kind ? `<span class="stop-kind">${esc(kind)}</span>` : ''}
      </div>
      <h4>${esc(stop.name)}</h4>
      ${description ? `<p class="stop-desc">${esc(description)}</p>` : ''}
      ${pinned ? '' : '<p class="stop-geo">Координаты не определены — точка есть в списке, но не отмечена на карте.</p>'}
    </div>
  </li>`;
}

function legHtml(leg) {
  return `<li class="leg">
    <div class="leg-rail"><span></span></div>
    <div class="leg-text">${esc(legText(leg))}</div>
  </li>`;
}

$('stops').addEventListener('click', (event) => {
  const item = event.target.closest('.stop');
  if (!item) return;
  document.querySelectorAll('.stop.on').forEach((node) => node.classList.remove('on'));
  item.classList.add('on');
  const marker = (markersByDay[day] || [])[Number(item.dataset.i)];
  if (marker && map) {
    map.flyTo(marker.getLatLng(), 11);
    marker.openPopup();
  }
});

function eventDates(event) {
  const start = event.date || '';
  const end = event.end_date || '';
  if (!start) return end ? fmtDate(end) : '';
  if (!end || String(start) === String(end)) return fmtDate(start);
  return `${fmtDate(start)} — ${fmtDate(end)}`;
}

// Events are a per-day stack of cards: they have no coordinates and never
// become map pins or stops. Missing fields fall back to neutral text.
function renderEvents(events) {
  const box = $('events-box');
  const grid = $('events');
  const list = Array.isArray(events) ? events : [];
  if (!list.length) {
    box.hidden = true;
    grid.innerHTML = '';
    return;
  }
  box.hidden = false;
  grid.innerHTML = list.map((value) => {
    const event = value && typeof value === 'object' ? value : {};
    const title = String(event.title || '').trim() || 'Событие без названия';
    const place = String(event.place || '').trim() || 'Место уточняется';
    const dates = eventDates(event);
    const url = String(event.url || '');
    const link = /^https?:\/\//i.test(url)
      ? `<a class="event-link" href="${esc(url)}" target="_blank" rel="noopener noreferrer">Подробнее <span aria-hidden="true">→</span></a>`
      : '';
    return `<article class="event-card">
      ${dates ? `<span class="event-date">${esc(dates)}</span>` : ''}
      <h3>${esc(title)}</h3>
      <p class="event-meta">${esc(place)}</p>
      ${link}
    </article>`;
  }).join('');
}

function renderLodging() {
  const list = Array.isArray(plan?.lodging_options) && plan.lodging_options.length
    ? plan.lodging_options
    : (Array.isArray(plan?.lodging) ? plan.lodging : []);
  const box = $('lodging-box');
  if (!list.length) {
    box.hidden = true;
    $('lodging').innerHTML = '';
    return;
  }
  box.hidden = false;
  const chosen = new Set((plan.days || []).map((item) => item.lodging_id).filter(Boolean));
  $('lodging').innerHTML = list.map((item, index) => {
    const kind = lodgingKindRu[item.kind] || item.kind || 'жильё';
    const price = item.price_per_night
      ? `${Number(item.price_per_night).toLocaleString('ru-RU')} ₽<small>за ночь</small>`
      : 'цена за ночь не указана';
    const tag = chosen.has(item.id) ? 'в плане' : (index === 0 ? 'рекомендация' : '');
    return `<article class="lodge${chosen.has(item.id) ? ' chosen' : ''}">
      <div class="lodge-top">
        <span class="lodge-num">${String(index + 1).padStart(2, '0')}</span>
        ${tag ? `<span class="lodge-tag">${esc(tag)}</span>` : ''}
      </div>
      <h3>${esc(item.name)}</h3>
      <p>${esc(item.settlement || 'Приморье')} · ${esc(kind)}</p>
      <div class="lodge-price">${price}</div>
    </article>`;
  }).join('');
}
