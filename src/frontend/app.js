/* vibecode frontend — vanilla JS SPA, no build step.
   Reads the SSE stream from POST /api/plan (same-origin only), renders days,
   lodging and a lazily created Leaflet map. Map pins are accepted only when
   the backend place has finite coordinates inside Primorye; water-body
   centroids and (0,0) are ignored so a pin can never land in the sea. */
const API = '';
const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

const MAX_DAYS = 7;
// Must match PRIMORYE_BBOX in src/domain/geo.rs.
const PRIMORYE_BBOX = { south: 42.2, west: 130.0, north: 48.5, east: 139.1 };
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

// A plan item must be a real, finite point inside Primorye. Unknown places
// arrive as (0,0) and must never become a pin in the sea or zoom the map out.
function mapPoint(stop) {
  const lat = Number(stop.lat ?? stop.latitude);
  const lon = Number(stop.lon ?? stop.lng ?? stop.longitude);
  if (!Number.isFinite(lat) || !Number.isFinite(lon)) return null;
  if (lat < PRIMORYE_BBOX.south || lat > PRIMORYE_BBOX.north
    || lon < PRIMORYE_BBOX.west || lon > PRIMORYE_BBOX.east) return null;
  if (Math.abs(lat) < 0.0001 && Math.abs(lon) < 0.0001) return null;
  if (waterName(stop.name)) return null;
  return [lat, lon];
}

function waterName(name) {
  const lower = String(name || '').toLowerCase();
  const parenthetical = lower.match(/\(([^)]*)\)/);
  const water = /^(бухт|озер|озёр|водохранилищ|пруд|пролив|ковш|лиман|затон|стариц|проток|канал)|^(залив|река|реки|реке|реку|рекой|море|моря|морю)$/;
  if (parenthetical && parenthetical[1].split(/[^\p{L}]+/u).some((word) => water.test(word))) return true;
  const words = lower.split(/[^\p{L}]+/u).filter(Boolean);
  return water.test(words[0] || '') || water.test(words[words.length - 1] || '');
}

// ---------- маршрут и АЗС ----------
const ROUTE_COLOR = '#0f766e';

// A leg geometry arrives as a GeoJSON LineString serialized to a JSON string.
// Both the GeoJSON object ({coordinates:[[lon,lat],...]}) and a bare array of
// [lon,lat] pairs are accepted. Malformed values, short lines and any point
// outside Primorye reject the whole line: a broken geometry draws nothing
// instead of a line across (or out of) the map.
function legLine(leg) {
  let raw = leg && leg.geometry;
  if (typeof raw === 'string') {
    try { raw = JSON.parse(raw); } catch { return null; }
  }
  if (!raw || typeof raw !== 'object') return null;
  const coords = Array.isArray(raw) ? raw : raw.coordinates;
  if (!Array.isArray(coords) || coords.length < 2) return null;
  const line = [];
  for (const pair of coords) {
    if (!Array.isArray(pair) || pair.length < 2) return null;
    const lon = Number(pair[0]);
    const lat = Number(pair[1]);
    if (!Number.isFinite(lat) || !Number.isFinite(lon)) return null;
    if (lat < PRIMORYE_BBOX.south || lat > PRIMORYE_BBOX.north
      || lon < PRIMORYE_BBOX.west || lon > PRIMORYE_BBOX.east) return null;
    line.push([lat, lon]);
  }
  return line;
}

// Fuel stations are an optional per-day extra (`day.fuel[]`); a missing or
// malformed list simply means "no fuel markers". Prices only exist in the
// demo fixture, so every price label is explicitly marked as demo.
const FUEL_GRADES = [['price_ai95', 'АИ-95'], ['price_ai92', 'АИ-92'], ['price_dt', 'ДТ']];
const FUEL_GLYPH = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4.5 21V5.5A2.5 2.5 0 0 1 7 3h4.5A2.5 2.5 0 0 1 14 5.5V21"/><path d="M3 21h12.5"/><path d="M6.7 8.6h5.6"/><path d="M14 12.6h1.8a2 2 0 0 1 2 2v4.2a1.6 1.6 0 0 0 3.2 0v-7.1L18.2 9"/></svg>';

function fuelName(station) {
  const name = String(station?.name || '').trim() || 'без названия';
  return /^азс(?![\p{L}])/iu.test(name) ? name : `АЗС ${name}`;
}

function fuelGrades(station) {
  return Array.isArray(station?.fuel) ? station.fuel.filter(Boolean).map(String) : [];
}

function fuelPrices(station) {
  const entries = [];
  for (const [key, label] of FUEL_GRADES) {
    const price = Number(station?.[key]);
    if (Number.isFinite(price) && price > 0) entries.push([label, price]);
  }
  return entries;
}

function fuelTooltipText(station) {
  const prices = fuelPrices(station);
  const price = prices.length ? ` · ${prices[0][0]} ${prices[0][1]} ₽/л (демо)` : '';
  return `${esc(fuelName(station))}${esc(price)}`;
}

function fuelPopupHtml(station) {
  const grades = fuelGrades(station);
  const prices = fuelPrices(station);
  const lines = [`<b>${esc(fuelName(station))}</b>`];
  if (grades.length) lines.push(`<span>Продаёт: ${esc(grades.join(', '))}</span>`);
  if (prices.length) {
    const text = prices.map(([label, price]) => `${label} — ${price} ₽/л`).join(' · ');
    lines.push(`<span>${esc(text)} (демо)</span>`);
  }
  lines.push(`<span>${prices.length ? 'Цены демонстрационные — уточняйте на АЗС' : 'Цены уточняйте на АЗС'}</span>`);
  return lines.join('<br>');
}

// ---------- карта ----------
let map = null;
let layer = null;
let markers = [];
let mapTilesLoaded = false;

function fitMap() {
  if (map) map.invalidateSize();
}

function initMap() {
  if (map || !window.L) return;
  // Touch devices keep one-finger page scrolling; the map is panned with the
  // zoom buttons. Desktop wheel zoom works only while the cursor is over it.
  const touchMap = navigator.maxTouchPoints > 0 || window.matchMedia('(any-pointer: coarse)').matches;
  map = L.map('map', {
    zoomControl: true,
    attributionControl: false,
    scrollWheelZoom: true,
    wheelDebounceTime: 40,
    wheelPxPerZoomLevel: 90,
    dragging: !touchMap,
    touchZoom: !touchMap,
    doubleClickZoom: !touchMap,
    boxZoom: false,
    keyboard: false,
    tapHold: false,
    preferCanvas: true,
    minZoom: 7,
    maxBounds: [[41.5, 128.5], [49.5, 141.0]],
    maxBoundsViscosity: 1.0,
  }).setView([43.7, 133.5], 8);
  if (touchMap) map.getContainer().style.touchAction = 'pan-y';

  const mapLoading = $('map-loading');
  const tiles = L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
    maxZoom: 18,
    keepBuffer: 1,
    updateWhenIdle: true,
    detectRetina: false,
    noWrap: true,
    bounds: L.latLngBounds([[41.5, 128.5], [49.5, 141.0]]),
  });
  tiles.on('tileload', () => {
    if (mapTilesLoaded) return;
    mapTilesLoaded = true;
    if (mapLoading) mapLoading.classList.add('hidden');
  });
  tiles.addTo(map);
  layer = L.layerGroup().addTo(map);
  map.whenReady(fitMap);
  setTimeout(() => {
    if (!mapTilesLoaded && mapLoading) {
      mapLoading.textContent = 'Карта недоступна — точки маршрута загружены';
    }
  }, 6000);
}

// Do not download tiles or create Leaflet layers until the map is near the
// viewport. This removes the biggest initial-load and scroll cost.
const mapCard = document.querySelector('.map-card');
if (mapCard && 'IntersectionObserver' in window) {
  const observer = new IntersectionObserver((entries) => {
    if (entries.some((entry) => entry.isIntersecting)) {
      observer.disconnect();
      initMap();
    }
  }, { rootMargin: '500px' });
  observer.observe(mapCard);
} else {
  setTimeout(initMap, 0);
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
  markers = [];
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
  // Summary не показываем: текст модели описывал служебные детали («51 объект
  // из Википедии», «план состоит из sights/museum/historic», «данные
  // демонстрационные»). Дни, предупреждения и погода и так видны ниже.
  const summary = $('p-sum');
  if (summary) { summary.textContent = ''; summary.hidden = true; }

  const intent = plan.intent || {};
  const meta = [];
  if (intent.start_date) {
    meta.push(`<span class="pill pill-date">${esc(fmtRange(intent.start_date, intent.end_date))}</span>`);
  }
  const transport = plan.transport || intent.transport;
  if (transport) meta.push(`<span class="pill">${esc(transportRu[transport] || transport)}</span>`);
  if (plan.total_km) meta.push(`<span class="pill">${(+plan.total_km).toFixed(0)} км</span>`);
  for (const weather of (plan.weather || []).slice(0, 3)) {
    meta.push(`<span class="pill pill-weather">${esc(fmtDate(weather.date))} · ${esc(weather.t_min ?? '')}…${esc(weather.t_max ?? '')}° · ${esc(weather.desc || '')}</span>`);
  }
  // "Убрано повторяющихся точек: 9" is an internal counter, not something the
  // traveller needs; the remaining warnings stay.
  const hiddenWarnings = /повтор|дубл/i;
  const warnings = [...new Set((plan.warnings || [])
    .map((w) => w.message || w)
    .filter((message) => !hiddenWarnings.test(message)))];
  for (const warning of warnings) meta.push(`<span class="pill pill-warn">${esc(warning)}</span>`);
  $('p-meta').innerHTML = meta.join('');

  const days = plan.days || [];
  if (day >= days.length) day = 0;
  $('tabs').innerHTML = days.map((item, index) =>
    `<button class="tab" role="tab" data-i="${index}" aria-selected="${index === day}">
      <span>День ${index + 1}</span><small>${esc(fmtDate(item.date))}</small>
    </button>`).join('');
  if (!map) initMap();
  renderDay();
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

  if (layer) layer.clearLayers();
  markers = [];
  if (!map || !window.L) return;
  // Route geometry of the active day first: Leaflet keeps paths in the overlay
  // pane, below the marker pane, so the line is drawn under both marker types.
  // A leg without usable geometry draws nothing.
  legs.forEach((leg) => {
    const line = legLine(leg);
    if (!line) return;
    L.polyline(line, {
      color: ROUTE_COLOR,
      weight: 4,
      opacity: 0.85,
      lineCap: 'round',
      lineJoin: 'round',
      interactive: false,
    }).addTo(layer);
  });
  // Fuel stations of the active day: orange square markers with a tooltip and
  // a popup. They are independent of the stop list and are cleared with the
  // layer whenever another day is rendered.
  const fuel = Array.isArray(current.fuel) ? current.fuel : [];
  fuel.forEach((station) => {
    const point = mapPoint(station);
    if (!point) return;
    L.marker(point, {
      icon: L.divIcon({
        className: 'pin-wrap',
        html: `<div class="fuel-pin">${FUEL_GLYPH}</div>`,
        iconSize: [22, 22],
        iconAnchor: [11, 11],
      }),
    })
      .bindTooltip(fuelTooltipText(station), { direction: 'top', offset: [0, -12] })
      .bindPopup(fuelPopupHtml(station))
      .addTo(layer);
  });
  const points = [];
  items.forEach((stop, index) => {
    const point = mapPoint(stop);
    if (!point) return;
    points.push(point);
    const [lat, lon] = point;
    const time = fmtTime(stop.start || stop.time);
    const popup = `<b>${esc(stop.name)}</b>${time ? `<br><span>${esc(time)}</span>` : ''}`;
    const marker = L.marker([lat, lon], {
      icon: L.divIcon({
        className: 'pin-wrap',
        html: `<div class="pin"><span>${index + 1}</span></div>`,
        iconSize: [34, 42],
        iconAnchor: [17, 40],
      }),
    }).bindPopup(popup).addTo(layer);
    markers[index] = marker;
  });
  if (points.length) {
    map.fitBounds(L.latLngBounds(points).pad(0.25), { maxZoom: 12 });
  }
}

function stopDescription(stop) {
  if (stop.note) return stop.note;
  const parts = [];
  const kind = kindRu[stop.kind] || 'точка маршрута';
  parts.push(kind.charAt(0).toUpperCase() + kind.slice(1));
  if (stop.opening_hours) parts.push(`часы: ${stop.opening_hours}`);
  return parts.join(' · ');
}

function stopHtml(stop, index) {
  const time = fmtTime(stop.start || stop.time);
  const end = fmtTime(stop.end);
  const range = time && end ? `${time}–${end}` : time;
  const kind = kindRu[stop.kind] || '';
  const description = stopDescription(stop);
  return `<li class="stop" data-i="${index}" data-place-id="${esc(stop.place_id || '')}">
    <div class="stop-rail"><span class="stop-num">${String(index + 1).padStart(2, '0')}</span></div>
    <div class="stop-body">
      <div class="stop-top">
        ${range ? `<span class="stop-time">${esc(range)}</span>` : ''}
        ${kind ? `<span class="stop-kind">${esc(kind)}</span>` : ''}
      </div>
      <h4>${esc(stop.name)}</h4>
      ${description ? `<p class="stop-desc">${esc(description)}</p>` : ''}
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
  const marker = markers[Number(item.dataset.i)];
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
    // Цену не показываем: у нас она демонстрационная.
    const price = '';
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

