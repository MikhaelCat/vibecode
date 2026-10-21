/* vibecode frontend — SSE-клиент для POST /api/plan.
   API всегда same-origin: произвольный адрес из query string не принимается. */
const API = '';
const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

const MAX_DAYS = 7;
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
  food: 'еда',
  transfer: 'переезд',
  rest: 'отдых',
};
const lodgingKindRu = { hotel: 'отель', guesthouse: 'гостевой дом', apartment: 'апартаменты', hostel: 'хостел' };
const MONTHS = ['янв', 'фев', 'мар', 'апр', 'мая', 'июн', 'июл', 'авг', 'сен', 'окт', 'ноя', 'дек'];

// ---------- карта ----------
let map = null;
let layer = null;
let markers = [];
let mapTilesLoaded = false;
if (window.L) {
  // Wheel zoom hijacks page scrolling and touch drags trap mobile users on the
  // map. Keep zoom buttons; allow wheel and one-finger scrolling everywhere.
  const compactMap = window.matchMedia('(max-width: 760px)').matches;
  map = L.map('map', {
    zoomControl: true,
    attributionControl: false,
    scrollWheelZoom: false,
    dragging: !compactMap,
    touchZoom: !compactMap,
    doubleClickZoom: !compactMap,
    boxZoom: false,
  }).setView([44.0, 133.0], 7);
  if (compactMap) map.getContainer().style.touchAction = 'pan-y';
  const mapLoading = $('map-loading');
  const tiles = L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', { maxZoom: 18 });
  tiles.on('tileload', () => {
    if (mapTilesLoaded) return;
    mapTilesLoaded = true;
    if (mapLoading) mapLoading.classList.add('hidden');
  });
  tiles.addTo(map);
  layer = L.layerGroup().addTo(map);
  setTimeout(() => {
    if (!mapTilesLoaded && mapLoading) {
      mapLoading.textContent = 'Карта недоступна — точки маршрута загружены';
    }
  }, 6000);
}

function fitMap() {
  if (map) map.invalidateSize();
}
if (map) {
  map.whenReady(fitMap);
  window.addEventListener('resize', fitMap);
  const card = document.querySelector('.map-card');
  if (window.ResizeObserver && card) new ResizeObserver(fitMap).observe(card);
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
    : 'Транспорт тоже возьмём из текста.';
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
  $('result').hidden = true;
  $('lodging-box').hidden = true;
  $('lodging').innerHTML = '';
  $('stops').innerHTML = '';
  $('tabs').innerHTML = '';
  $('day-name').textContent = '';
  $('day-facts').innerHTML = '';
  $('day-kicker').textContent = 'День 1';
  $('map-caption').textContent = 'Приморский край · карта OSM';
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
  box.hidden = false;
  box.innerHTML = `<div><b>Запрос не по теме.</b> ${esc(data.message || data.reason || 'Я планирую только поездки по Приморскому краю.')}</div>
    <button type="button" class="btn btn-small" id="refusal-example">Попробовать пример</button>`;
  const example = box.querySelector('#refusal-example');
  if (example) example.addEventListener('click', () => {
    $('q').value = EXAMPLES[0];
    updateCounter();
    requestPlan(EXAMPLES[0]);
  });
}

function showError(message) {
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

  if (layer) layer.clearLayers();
  markers = [];
  if (!map || !window.L) return;
  const points = [];
  items.forEach((stop, index) => {
    const lat = stop.lat ?? stop.latitude;
    const lon = stop.lon ?? stop.lng ?? stop.longitude;
    if (lat == null || lon == null) return;
    points.push([lat, lon]);
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
  if (points.length > 1) {
    L.polyline(points, { color: '#0f766e', weight: 3, opacity: 0.85, lineJoin: 'round' }).addTo(layer);
  }
  if (points.length) {
    map.fitBounds(L.latLngBounds(points).pad(0.25), { maxZoom: 12 });
  }
  $('map-caption').textContent = items.length
    ? `День ${day + 1} · ${items.length} ${items.length === 1 ? 'точка' : 'точки'}`
    : 'Приморский край · карта OSM';
}

function sourceInfo(placeId) {
  const id = String(placeId || '');
  if (id.startsWith('wikipedia:')) {
    const page = id.slice('wikipedia:'.length);
    return /^\d+$/.test(page)
      ? { label: 'Wikipedia', url: `https://ru.wikipedia.org/?curid=${page}` }
      : { label: 'Wikipedia', url: null };
  }
  const osm = id.match(/^osm:(node|way|relation)\/(\d+)$/);
  if (osm) {
    return {
      label: 'OpenStreetMap',
      url: `https://www.openstreetmap.org/${osm[1]}/${osm[2]}`,
    };
  }
  if (id.startsWith('nominatim:') || id.startsWith('photon:')) {
    return { label: 'OpenStreetMap', url: 'https://www.openstreetmap.org/' };
  }
  if (id.startsWith('mock:')) return { label: 'локальные данные', url: null };
  return { label: 'источник', url: null };
}

function stopHtml(stop, index) {
  const time = fmtTime(stop.start || stop.time);
  const end = fmtTime(stop.end);
  const range = time && end ? `${time}–${end}` : time;
  const kind = kindRu[stop.kind] || '';
  const source = stop.place_id ? sourceInfo(stop.place_id) : null;
  const sourceHtml = source
    ? (source.url
      ? `<a class="source" href="${esc(source.url)}" target="_blank" rel="noopener noreferrer" title="Открыть объект в источнике">Источник: ${esc(source.label)}</a>`
      : `<span class="source">Источник: ${esc(source.label)}</span>`)
    : '';
  return `<li class="stop" data-i="${index}" data-place-id="${esc(stop.place_id || '')}">
    <div class="stop-rail"><span class="stop-num">${String(index + 1).padStart(2, '0')}</span></div>
    <div class="stop-body">
      <div class="stop-top">
        ${range ? `<span class="stop-time">${esc(range)}</span>` : ''}
        ${kind ? `<span class="stop-kind">${esc(kind)}</span>` : ''}
      </div>
      <h4>${esc(stop.name)}</h4>
      ${stop.note ? `<p>${esc(stop.note)}</p>` : ''}
      ${sourceHtml}
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
      ? `${Number(item.price_per_night).toLocaleString('ru-RU')} ₽`
      : 'цена не указана';
    const tag = chosen.has(item.id) ? 'в плане' : (index === 0 ? 'рекомендация' : '');
    return `<article class="lodge${chosen.has(item.id) ? ' chosen' : ''}">
      <div class="lodge-top">
        <span class="lodge-num">${String(index + 1).padStart(2, '0')}</span>
        ${tag ? `<span class="lodge-tag">${esc(tag)}</span>` : ''}
      </div>
      <h3>${esc(item.name)}</h3>
      <p>${esc(item.settlement || 'Приморье')} · ${esc(kind)}</p>
      <div class="lodge-price">${esc(price)}<small>за ночь</small></div>
    </article>`;
  }).join('');
}

