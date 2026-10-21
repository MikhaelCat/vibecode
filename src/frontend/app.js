/* vibecode frontend — SSE-клиент для POST /api/plan.
   ?api=http://127.0.0.1:8080 — адрес бэкенда при локальной разработке. */
const API = new URLSearchParams(location.search).get('api') || '';
const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"]/g, (c) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));

const EXAMPLES = [
  'Хочу на выходные по Приморью на машине, не больше двух дней',
  'Один день у моря возле Владивостока, без долгих переездов',
  'Три дня: маяки, бухты и хорошая погода',
  'Что посмотреть на острове Русский с детьми?',
];
const STAGES = { guard: 'Проверяю тему', intent: 'Разбираю запрос', fallback: 'Упрощаю план', plan: 'Собираю план', validation: 'Проверяю план', done: 'Готово' };

// ---------- карта ----------
const map = L.map('map', { zoomControl: true }).setView([44.0, 133.0], 7);
L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
  maxZoom: 18, attribution: '© участники <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>',
}).addTo(map);
const layer = L.layerGroup().addTo(map);
let markers = [];

// ---------- состояние ----------
let plan = null, day = 0;

// ---------- старт ----------
$('chips').innerHTML = EXAMPLES.map((t) => `<button type="button" class="chip">${esc(t)}</button>`).join('');
$('chips').addEventListener('click', (e) => {
  if (e.target.classList.contains('chip')) { $('q').value = e.target.textContent; $('q').focus(); }
});
$('token').value = localStorage.getItem('app_token') || '';
$('form').addEventListener('submit', (e) => { e.preventDefault(); run($('q').value.trim()); });

// ?query=...&autorun=1 — shareable query and headless UI test hook.
const params = new URLSearchParams(location.search);
const prefill = params.get('query');
if (prefill) {
  $('q').value = prefill;
  if (params.get('autorun') === '1') setTimeout(() => run(prefill), 0);
}
fetch(API + '/api/health').then((r) => r.json()).then((h) => {
  const el = $('health'); el.textContent = `${h.model || 'онлайн'}`; el.classList.add('ok');
}).catch(() => {});

// ---------- запуск ----------
async function run(query) {
  if (!query) return;
  localStorage.setItem('app_token', $('token').value);
  reset(); $('go').disabled = true;
  try {
    const headers = { 'content-type': 'application/json' };
    if ($('token').value) headers['x-app-token'] = $('token').value;
    const res = await fetch(API + '/api/plan', { method: 'POST', headers, body: JSON.stringify({ query }) });
    if (!res.ok) throw new Error(res.status === 401 ? 'Нужен токен доступа (APP_TOKEN).' : `Сервер ответил ${res.status}.`);
    await readSSE(res.body, handle);
  } catch (err) {
    console.warn(err);
    stage('Бэкенд недоступен — показываю демо-данные', 'done');
    await demo();
  } finally { $('go').disabled = false; }
}

async function readSSE(body, cb) {
  const reader = body.getReader(), dec = new TextDecoder();
  let buf = '';
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true }).replace(/\r/g, '');
    let i;
    while ((i = buf.indexOf('\n\n')) >= 0) {
      const block = buf.slice(0, i); buf = buf.slice(i + 2);
      let ev = 'message', data = '';
      for (const line of block.split('\n')) {
        if (line.startsWith('event:')) ev = line.slice(6).trim();
        else if (line.startsWith('data:')) data += line.slice(5).trim();
      }
      if (data) { try { cb(ev, JSON.parse(data)); } catch { cb(ev, data); } }
    }
  }
}

// ---------- события ----------
function handle(ev, d) {
  if (ev === 'stage') stage(STAGES[d.stage] || d.label || d.name || d.stage || String(d));
  else if (STAGES[ev] && ev !== 'intent') stage(STAGES[ev], ev === 'done' ? 'done' : 'live');
  if (ev === 'intent') trace('intent', JSON.stringify(d));
  if (ev === 'tool_call') trace(d.name || d.tool || 'tool', JSON.stringify(d.args ?? d.input ?? {}));
  if (ev === 'tool_result') trace(d.name || d.tool || 'result', d.ok === false ? 'ошибка: ' + (d.error || '') : 'ок' + (d.ms ? ` · ${d.ms} мс` : ''), d.ok === false);
  if (ev === 'plan') { plan = d.plan || d; day = 0; renderPlan(); }
  if (ev === 'validation') renderChecks(d);
  if (ev === 'usage') $('usage').textContent = [d.total_tokens && d.total_tokens + ' ток.', d.cost_usd != null && '$' + (+d.cost_usd).toFixed(4)].filter(Boolean).join(' · ');
  if (ev === 'refusal') refuse(d);
}

// ---------- UI ----------
function reset() {
  $('stages').innerHTML = ''; $('trace').innerHTML = ''; $('usage').textContent = '';
  $('refusal').hidden = true; $('result').hidden = true; $('checks').innerHTML = '';
  layer.clearLayers(); markers = []; plan = null;
}
function stage(text, state = 'live') {
  const ul = $('stages');
  ul.querySelectorAll('.live').forEach((li) => { li.className = 'done'; });
  const last = ul.lastElementChild;
  if (last && last.textContent === text) { last.className = state; return; }
  ul.insertAdjacentHTML('beforeend', `<li class="${state}">${esc(text)}</li>`);
}
function trace(name, text, bad) {
  $('trace').insertAdjacentHTML('beforeend', `<li><span class="n">${esc(name)}</span><span class="${bad ? 'bad' : ''}">${esc(String(text).slice(0, 220))}</span></li>`);
}
function refuse(d) {
  $('stages').innerHTML = '';
  const r = $('refusal'); r.hidden = false;
  r.innerHTML = `<b>Запрос не по теме.</b> ${esc(d.message || d.reason || 'Я планирую только поездки по Приморскому краю.')}`;
}
const isMock = (o) => !!(o && (o.source === 'mock' || o.source === 'stale' || o.mock || o.is_mock));
const mockTag = (o) => (isMock(o) ? '<span class="mock">мок</span>' : '');
const fmtTime = (t) => (t ? String(t).slice(0, 5) : '');
const transportRu = { car: 'машина', foot: 'пешком', bike: 'велосипед', transit: 'автобус/поезд' };
const legText = (leg) => leg
  ? `${leg.mode === 'car' ? '🚗' : leg.mode === 'foot' ? '🚶' : leg.mode === 'bike' ? '🚲' : '🚌'} ${(+leg.distance_km || 0).toFixed(1)} км · ${leg.duration_min || 0} мин`
  : '';

function planItems(day) { return day.items || day.stops || []; }
function planLodging() { const l = plan?.lodging; return Array.isArray(l) ? l[0] : l; }

function renderPlan() {
  $('result').hidden = false;
  $('p-title').textContent = plan.title || 'Ваш маршрут';
  $('p-sum').textContent = plan.summary || '';
  const meta = [];
  const intent = plan.intent || {};
  if (intent.start_date) meta.push(`<span class="pill">${esc(intent.start_date)} — ${esc(intent.end_date || '')}</span>`);
  if (plan.transport || intent.transport) meta.push(`<span class="pill">${esc(transportRu[plan.transport || intent.transport] || plan.transport || '')}</span>`);
  if (plan.total_km) meta.push(`<span class="pill">${(+plan.total_km).toFixed(0)} км</span>`);
  if (plan.cost_estimate_rub) meta.push(`<span class="pill">≈ ${plan.cost_estimate_rub} ₽</span>`);
  (plan.weather || []).slice(0, 3).forEach((w) =>
    meta.push(`<span class="pill">${esc(w.date || '')} · ${esc(w.t_min ?? '')}…${esc(w.t_max ?? '')}° ${esc(w.desc || w.summary || '')}</span>`));
  const lod = planLodging();
  if (lod) meta.push(`<span class="pill">Ночёвка: ${esc(lod.name)}${mockTag(lod)}</span>`);
  (plan.warnings || []).forEach((w) => meta.push(`<span class="pill">⚠ ${esc(w.message || w)}</span>`));
  $('p-meta').innerHTML = meta.join('');
  const days = plan.days || [];
  $('tabs').innerHTML = days.map((d, i) =>
    `<button class="tab" role="tab" data-i="${i}" aria-selected="${i === day}">${esc(d.title || 'День ' + (i + 1))}</button>`).join('');
  renderDay();
}
$('tabs').addEventListener('click', (e) => {
  const b = e.target.closest('.tab'); if (!b) return;
  day = +b.dataset.i; renderPlan();
});

function renderDay() {
  const d = (plan.days || [])[day]; if (!d) return;
  const items = planItems(d);
  const legs = d.legs || [];
  let html = '';
  items.forEach((s, i) => {
    if (legs[i - 1]) html += `<li class="leg">${esc(legText(legs[i - 1]))}</li>`;
    const time = fmtTime(s.start || s.time);
    const end = fmtTime(s.end);
    const range = time && end ? `${time}–${end}` : time;
    html += `<li class="stop" data-i="${i}">
      <div class="t">${esc(range)}</div>
      <h4>${esc(s.name)}${mockTag(s)}</h4>
      ${s.note ? `<p>${esc(s.note)}</p>` : ''}
      ${s.place_id ? `<code title="Источник точки">${esc(s.place_id)}</code>` : ''}
    </li>`;
    if (s.leg) html += `<li class="leg">${esc(s.leg)}</li>`;
  });
  $('stops').innerHTML = html;
  layer.clearLayers(); markers = [];
  const pts = [];
  items.forEach((s, i) => {
    const lat = s.lat ?? s.latitude, lon = s.lon ?? s.lng ?? s.longitude;
    if (lat == null || lon == null) return;
    pts.push([lat, lon]);
    const m = L.marker([lat, lon], { icon: L.divIcon({ className: '', html: `<div class="pin">${i + 1}</div>`, iconSize: [26, 26] }) })
      .bindPopup(`<b>${esc(s.name)}</b>`).addTo(layer);
    markers[i] = m;
  });
  const line = d.geometry && d.geometry.length ? d.geometry : pts;
  if (line.length > 1) L.polyline(line, { color: '#2e5e4e', weight: 4, opacity: .85 }).addTo(layer);
  if (pts.length) map.fitBounds(L.latLngBounds(pts).pad(.25), { maxZoom: 12 });
}
$('stops').addEventListener('click', (e) => {
  const li = e.target.closest('.stop'); if (!li) return;
  document.querySelectorAll('.stop.on').forEach((x) => x.classList.remove('on'));
  li.classList.add('on');
  const m = markers[+li.dataset.i]; if (m) { map.flyTo(m.getLatLng(), 11); m.openPopup(); }
});

function renderChecks(v) {
  const list = v.violations || v.checks || v.results || [];
  const hard = v.hard_failed === true || list.some((c) => c.severity === 'hard' || c.ok === false || c.passed === false);
  const passed = v.checks_passed != null ? v.checks_passed : list.filter((c) => !(c.ok === false || c.passed === false)).length;
  const total = v.checks_run != null ? v.checks_run : list.length;
  const verdict = hard
    ? '<div class="verdict">Есть блокирующие нарушения — план показан с предупреждениями.</div>'
    : `<div class="verdict">Проверки пройдены${total ? `: ${passed}/${total}` : ''}${v.score != null ? ` · score ${(+v.score).toFixed(2)}` : ''}.</div>`;
  $('checks').innerHTML = verdict + list.map((c) => {
    const ok = !(c.severity === 'hard' || c.ok === false || c.passed === false);
    return `<span class="ck ${ok ? 'ok' : 'fail'}" title="${esc(c.message || c.msg || '')}">${esc(c.code || c.id || 'V')} ${ok ? '✓' : '✗'}</span>`;
  }).join('');
}

// ---------- демо без бэкенда ----------
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function demo() {
  handle('stage', { label: 'Разбираю запрос' }); await sleep(500);
  handle('intent', { region: 'Приморский край', days: 2 });
  handle('stage', { label: 'Ищу места и погоду' });
  handle('tool_call', { name: 'get_nearby_attractions', args: { lat: 43.12, lon: 131.89 } }); await sleep(400);
  handle('tool_result', { name: 'get_nearby_attractions', ok: true, ms: 380 });
  handle('tool_call', { name: 'get_weather', args: { days: 2 } }); await sleep(300);
  handle('tool_result', { name: 'get_weather', ok: true, ms: 210 });
  handle('stage', { label: 'Собираю план' }); await sleep(500);
  handle('plan', { title: 'Выходные во Владивостоке (демо)', summary: 'Координаты приблизительные, это пример для проверки вёрстки.',
    weather: [{ date: 'Сб', t_min: 9, t_max: 15, desc: 'облачно' }, { date: 'Вс', t_min: 10, t_max: 17, desc: 'ясно' }],
    lodging: { name: 'Гостиница в центре', mock: true },
    days: [
      { title: 'Центр и маяки', stops: [
        { name: 'Золотой мост', lat: 43.1176, lon: 131.9117, time: '10:00', note: 'Смотровая и вид на бухту Золотой Рог.', place_id: 'osm:demo-1', leg: '🚗 15 мин · 6 км' },
        { name: 'Набережная Спортивной гавани', lat: 43.1332, lon: 131.9094, time: '12:30', note: 'Прогулка и обед у воды.', place_id: 'osm:demo-2', leg: '🚗 40 мин · 22 км' },
        { name: 'Токаревский маяк', lat: 43.0225, lon: 131.8625, time: '15:30', note: 'Узкая коса, лучший закат дня.', place_id: 'osm:demo-3' }] },
      { title: 'Остров Русский', stops: [
        { name: 'Русский мост', lat: 43.0239, lon: 131.9139, time: '10:30', note: 'Остановка у подножия вантов.', place_id: 'osm:demo-4', leg: '🚗 20 мин · 12 км' },
        { name: 'Приморский океанариум', lat: 43.0225, lon: 131.9217, time: '12:00', note: 'Билеты по расписанию — данные мок.', place_id: 'mock:demo-5', mock: true }] }] });
  handle('validation', { passed: true, checks: ['V01', 'V02', 'V03', 'V04', 'V05'].map((id) => ({ id, ok: true })) });
  handle('usage', { total_tokens: 0, cost_usd: 0 });
  handle('done', {});
}
