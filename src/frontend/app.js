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
const TOOL_RU = {
  search_places: 'Поиск мест',
  get_nearby_attractions: 'Википедия рядом',
  geocode: 'Геокодинг',
  get_weather: 'Погода',
  get_route: 'Маршрут',
  find_lodging: 'Жильё',
  find_transport: 'Транспорт',
  find_fuel: 'АЗС',
  find_events: 'События',
  submit_plan: 'Сборка плана',
};
const transportRu = { car: 'машина', foot: 'пешком', bike: 'велосипед', transit: 'автобус/поезд' };
const kindRu = {
  sight: 'достопримечательность',
  museum: 'музей',
  nature: 'природа',
  viewpoint: 'смотровая',
  park: 'парк',
  historic: 'история',
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
if (window.L) {
  map = L.map('map', { zoomControl: true, attributionControl: false }).setView([44.0, 133.0], 7);
  L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', { maxZoom: 18 }).addTo(map);
  layer = L.layerGroup().addTo(map);
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
let datesTouched = true;
let running = false;
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
function dayTitle(title) {
  if (!title) return 'Маршрут дня';
  return title.replace(/^День\s+\d+\s*[—–-]\s*/i, '') || title;
}

function defaultDates() {
  const start = addDays(new Date(), 1);
  const end = addDays(start, 2);
  $('start_date').value = isoDate(start);
  $('end_date').value = isoDate(end);
  $('start_date').min = isoDate(new Date());
  syncDateBounds();
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
  datesTouched = true;
  syncDateBounds();
  updateDateHint();
}

function updateDateHint() {
  const hint = document.querySelector('.hint');
  if (!hint) return;
  hint.textContent = datesTouched
    ? 'Даты из формы главнее текста запроса — план будет ровно на этот период.'
    : 'Даты не тронуты — возьмём их из текста запроса. Выбрали вручную — форма главнее.';
}

defaultDates();
$('start_date').addEventListener('change', clampDates);
$('end_date').addEventListener('change', clampDates);

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
  run($('q').value.trim());
});
$('retry').addEventListener('click', () => run($('q').value.trim()));

// ?query=...&autorun=1 — shareable query and headless UI test hook.
const params = new URLSearchParams(location.search);
const prefill = params.get('query');
if (prefill) {
  $('q').value = prefill;
  updateCounter();
  if (params.get('autorun') === '1') setTimeout(() => run(prefill), 0);
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
async function run(query) {
  if (!query || running) return;
  running = true;
  localStorage.setItem('app_token', $('token').value);
  reset();
  setBusy(true);

  try {
    const headers = { 'content-type': 'application/json' };
    if ($('token').value) headers['x-app-token'] = $('token').value;
    const payload = { query, transport: $('transport').value };
    if (datesTouched) {
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
    if (!datesTouched && data.start_date && data.end_date) {
      $('start_date').value = data.start_date;
      $('end_date').value = data.end_date;
      syncDateBounds();
    }
    trace(
      'Разбор запроса',
      `${fmtRange(data.start_date, data.end_date)} · ${transportRu[data.transport] || data.transport || 'транспорт'}`
    );
  }
  if (event === 'tool_call') {
    const name = data.name || data.tool || 'tool';
    trace(TOOL_RU[name] || name, JSON.stringify(data.args ?? data.input ?? {}));
  }
  if (event === 'tool_result') {
    const name = data.name || data.tool || 'result';
    const text = data.ok === false
      ? `ошибка: ${data.error || 'нет данных'}`
      : ['ок', data.ms ? `${data.ms} мс` : '', data.cached ? 'из кэша' : ''].filter(Boolean).join(' · ');
    trace(TOOL_RU[name] || name, text, data.ok === false);
  }
  if (event === 'plan') {
    plan = data.plan || data;
    day = 0;
    stage(STAGES.plan, 'done');
    renderPlan();
  }
  if (event === 'validation') {
    stage(STAGES.validation, 'done');
    renderChecks(data);
  }
  if (event === 'usage') {
    const usage = [];
    if (data.total_tokens) usage.push(`${Number(data.total_tokens).toLocaleString('ru-RU')} токенов`);
    if (data.prompt_tokens) usage.push(`${Number(data.prompt_tokens).toLocaleString('ru-RU')} вход`);
    if (data.completion_tokens) usage.push(`${Number(data.completion_tokens).toLocaleString('ru-RU')} выход`);
    if (data.cost_usd != null) usage.push(`$${(+data.cost_usd).toFixed(4)}`);
    $('usage').textContent = usage.join(' · ');
  }
  if (event === 'refusal') {
    stage(STAGES.done, 'done');
    refuse(data);
  }
  if (event === 'done') stage(STAGES.done, 'done');
}

// ---------- UI ----------
function reset() {
  $('stages').innerHTML = '';
  $('trace').innerHTML = '';
  $('usage').textContent = '';
  $('refusal').hidden = true;
  $('refusal').innerHTML = '';
  $('error').hidden = true;
  $('result').hidden = true;
  $('checks').innerHTML = '';
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

function trace(name, text, bad) {
  $('trace').insertAdjacentHTML(
    'beforeend',
    `<li><span class="n">${esc(name)}</span><span class="${bad ? 'bad' : ''}">${esc(String(text).slice(0, 240))}</span></li>`
  );
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
    run(EXAMPLES[0]);
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
  $('day-name').textContent = dayTitle(current.title);

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

function stopHtml(stop, index) {
  const time = fmtTime(stop.start || stop.time);
  const end = fmtTime(stop.end);
  const range = time && end ? `${time}–${end}` : time;
  const kind = kindRu[stop.kind] || '';
  return `<li class="stop" data-i="${index}">
    <div class="stop-rail"><span class="stop-num">${String(index + 1).padStart(2, '0')}</span></div>
    <div class="stop-body">
      <div class="stop-top">
        ${range ? `<span class="stop-time">${esc(range)}</span>` : ''}
        ${kind ? `<span class="stop-kind">${esc(kind)}</span>` : ''}
      </div>
      <h4>${esc(stop.name)}</h4>
      ${stop.note ? `<p>${esc(stop.note)}</p>` : ''}
      ${stop.place_id ? `<code title="Источник точки">${esc(stop.place_id)}</code>` : ''}
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

function renderChecks(report) {
  const violations = report.violations || report.checks || report.results || [];
  const groups = new Map();
  for (const violation of violations) {
    const code = violation.code || violation.id || 'V';
    const group = groups.get(code) || { code, count: 0, bad: false, message: '' };
    group.count += 1;
    if (violation.severity === 'hard' || violation.ok === false || violation.passed === false) group.bad = true;
    if (!group.message) group.message = violation.message || violation.msg || '';
    groups.set(code, group);
  }
  const hard = report.hard_failed === true || [...groups.values()].some((group) => group.bad);
  const passed = report.checks_passed != null
    ? report.checks_passed
    : Math.max(0, (report.checks_run || groups.size) - groups.size);
  const total = report.checks_run != null ? report.checks_run : groups.size;

  const verdict = hard
    ? '<div class="verdict verdict-warn">Есть блокирующие нарушения — план показан с предупреждениями.</div>'
    : `<div class="verdict verdict-ok">Проверки пройдены${total ? `: ${passed}/${total}` : ''}${report.score != null ? ` · score ${(+report.score).toFixed(2)}` : ''}.</div>`;

  const chips = [...groups.values()].map((group) =>
    `<span class="ck ${group.bad ? 'fail' : 'ok'}" title="${esc(group.message)}">
      ${esc(group.code)} <b>${group.count > 1 ? group.count : '✓'}</b>
    </span>`).join('');

  $('checks').innerHTML = verdict + (chips ? `<div class="check-chips">${chips}</div>` : '');
}
