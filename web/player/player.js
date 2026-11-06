// Qivomedia web player — pairing, offline-first manifest, multi-zone rendering,
// realtime push, interrupts (triggers), proof-of-play.
import { activeLayouts, itemPlayable, fillTemplate } from "/shared/schedule.js";

const qs = new URLSearchParams(location.search);
const PREVIEW = qs.get("preview");
const PREVIEW_TOKEN = qs.get("token");
const LS = { key: "qivo.device_key", id: "qivo.display_id", manifest: "qivo.manifest", pop: "qivo.pop_queue" };
const viewport = document.getElementById("viewport");
const startedAt = Date.now();

let manifest = null;
let current = { key: "", layouts: [], idx: 0, stage: null, until: 0 };
let interrupt = null; // { stage, until, layout_id }
let ws = null;
let wsBackoff = 1000;

// ---------------- utils ----------------
const el = (tag, cls, html) => { const e = document.createElement(tag); if (cls) e.className = cls; if (html != null) e.innerHTML = html; return e; };
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const deviceKey = () => localStorage.getItem(LS.key);

async function api(path, opts = {}) {
  const headers = { "Content-Type": "application/json", ...(opts.headers || {}) };
  if (PREVIEW_TOKEN) headers.Authorization = `Bearer ${PREVIEW_TOKEN}`;
  else if (deviceKey()) headers["X-Device-Key"] = deviceKey();
  const res = await fetch(path, { ...opts, headers });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) { const e = new Error(body.error || res.statusText); e.status = res.status; throw e; }
  return body;
}

// ---------------- system screens ----------------
function sysScreen(inner) {
  document.querySelectorAll(".sys").forEach((n) => n.remove());
  if (!inner) return;
  const s = el("div", "sys", `<div class="brand"><i></i>Qivomedia</div>${inner}`);
  document.body.appendChild(s);
  return s;
}

function showPairing(code) {
  sysScreen(`
    <p>Pair this screen in the dashboard → <b>Displays → Add screen</b> and enter:</p>
    <div class="code">${esc(code)}</div>
    <p class="small">${esc(location.origin)} · waiting for pairing…</p>`);
}

function overlay(html, ms = 8000) {
  const o = el("div", "overlay", html);
  document.body.appendChild(o);
  setTimeout(() => o.remove(), ms);
}

// ---------------- media cache ----------------
async function syncMediaCache(m) {
  if (!("caches" in window)) return;
  const wanted = new Set(Object.values(m.media || {}).map((x) => x.url));
  const cache = await caches.open("qivo-media");
  const have = new Set((await cache.keys()).map((r) => new URL(r.url).pathname));
  const missing = [...wanted].filter((u) => !have.has(u));
  let done = 0;
  for (const url of missing) {
    try {
      const res = await fetch(url, { cache: "no-store" });
      if (res.ok) await cache.put(url, res);
    } catch (e) { console.warn("cache failed", url, e); }
    done++;
    const bar = document.querySelector(".progress > div");
    if (bar) bar.style.width = `${Math.round((done / missing.length) * 100)}%`;
  }
  // Evict files no longer referenced.
  for (const url of have) if (!wanted.has(url)) await cache.delete(url);
}

// ---------------- proof of play ----------------
function popRecord(rec) {
  if (PREVIEW) return;
  const q = JSON.parse(localStorage.getItem(LS.pop) || "[]");
  q.push(rec);
  localStorage.setItem(LS.pop, JSON.stringify(q.slice(-20000)));
}
async function flushPop() {
  if (PREVIEW) return;
  const q = JSON.parse(localStorage.getItem(LS.pop) || "[]");
  if (!q.length) return;
  const batch = q.slice(0, 2000);
  try {
    await api("/api/player/pop", { method: "POST", body: JSON.stringify(batch) });
    const rest = JSON.parse(localStorage.getItem(LS.pop) || "[]").slice(batch.length);
    localStorage.setItem(LS.pop, JSON.stringify(rest));
  } catch { /* offline: keep queue */ }
}

// ---------------- widgets ----------------
function renderItem(item, ctx) {
  const t = item.type;
  const data = ctx.data;
  if (t === "media") {
    const m = manifest.media?.[item.media_id];
    if (!m) return null;
    if (m.kind === "image") {
      const img = el("img", `fit-${item.fit || "contain"}`);
      img.src = m.url; img.alt = "";
      return { node: img };
    }
    if (m.kind === "video") {
      const v = el("video", `fit-${item.fit || "contain"}`);
      v.src = m.url; v.muted = item.mute !== false; v.playsInline = true; v.autoplay = true; v.preload = "auto";
      const ended = new Promise((res) => { v.onended = res; v.onerror = res; });
      v.play().catch(() => { v.muted = true; v.play().catch(() => {}); });
      return { node: v, ended: item.play_full !== false ? ended : null };
    }
    if (m.kind === "pdf" || m.kind === "html") {
      const f = el("iframe"); f.src = m.url; return { node: f };
    }
    if (m.kind === "audio") {
      const a = el("audio"); a.src = m.url; a.autoplay = true;
      const ended = new Promise((res) => { a.onended = res; a.onerror = res; });
      return { node: a, ended };
    }
    return null;
  }
  const color = item.color || "#ffffff";
  const bg = item.bg || "transparent";
  const size = item.size || 64;
  if (t === "text") {
    const align = item.align || "center";
    const just = { left: "flex-start", center: "center", right: "flex-end" }[align];
    const d = el("div", "w-text");
    Object.assign(d.style, { color, background: bg, fontSize: `${size}px`, justifyContent: just, alignItems: item.valign || "center", textAlign: align, fontWeight: item.bold ? 700 : 500 });
    d.textContent = fillTemplate(item.text, data);
    return { node: d };
  }
  if (t === "ticker") {
    const d = el("div", "w-ticker");
    Object.assign(d.style, { color, background: bg, fontSize: `${size}px` });
    const s = el("span"); s.textContent = fillTemplate(item.text, data);
    const speed = item.speed || 120; // px / second
    d.appendChild(s);
    requestAnimationFrame(() => { s.style.animationDuration = `${Math.max(4, (s.scrollWidth) / speed)}s`; });
    return { node: d };
  }
  if (t === "clock") {
    const d = el("div", "w-clock");
    Object.assign(d.style, { color, background: bg });
    const tick = () => {
      const now = new Date();
      const time = now.toLocaleTimeString(item.locale || undefined, { hour: "2-digit", minute: "2-digit", second: item.seconds ? "2-digit" : undefined, hour12: item.format === "12h" });
      const date = item.show_date === false ? "" : now.toLocaleDateString(item.locale || undefined, { weekday: "long", day: "numeric", month: "long" });
      d.innerHTML = `<div class="t" style="font-size:${size}px">${esc(time)}</div>${date ? `<div style="font-size:${Math.round(size * 0.32)}px;opacity:.8">${esc(date)}</div>` : ""}`;
    };
    tick();
    return { node: d, timer: setInterval(tick, 1000) };
  }
  if (t === "countdown") {
    const d = el("div", "w-countdown");
    Object.assign(d.style, { color, background: bg });
    const target = new Date(item.target || Date.now());
    const tick = () => {
      let s = Math.max(0, Math.floor((target - Date.now()) / 1000));
      const days = Math.floor(s / 86400); s %= 86400;
      const p = (n) => String(n).padStart(2, "0");
      const txt = `${days ? days + "d " : ""}${p(Math.floor(s / 3600))}:${p(Math.floor((s % 3600) / 60))}:${p(s % 60)}`;
      d.innerHTML = `${item.label ? `<div style="font-size:${Math.round(size * 0.35)}px;opacity:.85">${esc(fillTemplate(item.label, data))}</div>` : ""}<div class="t" style="font-size:${size}px">${txt}</div>`;
    };
    tick();
    return { node: d, timer: setInterval(tick, 1000) };
  }
  if (t === "weather") {
    const d = el("div", "w-weather");
    Object.assign(d.style, { color, background: bg });
    renderWeather(d, item, size);
    return { node: d };
  }
  if (t === "webpage") {
    const f = el("iframe");
    f.src = item.url;
    f.setAttribute("sandbox", "allow-scripts allow-same-origin allow-forms");
    f.setAttribute("allow", "autoplay");
    if (item.zoom && item.zoom !== 1) { f.style.transform = `scale(${item.zoom})`; f.style.transformOrigin = "0 0"; f.style.width = `${100 / item.zoom}%`; f.style.height = `${100 / item.zoom}%`; }
    return { node: f };
  }
  if (t === "html") {
    const f = el("iframe");
    f.setAttribute("sandbox", "allow-scripts");
    f.srcdoc = `<!doctype html><html><head><meta charset="utf-8"><style>html,body{margin:0;height:100%;overflow:hidden;background:transparent}</style></head><body>${fillTemplate(item.html, data)}</body></html>`;
    return { node: f };
  }
  return null;
}

const WMO = { 0: ["Clear", "☀️"], 1: ["Mostly clear", "🌤️"], 2: ["Partly cloudy", "⛅"], 3: ["Cloudy", "☁️"], 45: ["Fog", "🌫️"], 48: ["Fog", "🌫️"],
  51: ["Drizzle", "🌦️"], 53: ["Drizzle", "🌦️"], 55: ["Drizzle", "🌦️"], 61: ["Rain", "🌧️"], 63: ["Rain", "🌧️"], 65: ["Heavy rain", "🌧️"],
  71: ["Snow", "🌨️"], 73: ["Snow", "🌨️"], 75: ["Heavy snow", "❄️"], 80: ["Showers", "🌦️"], 81: ["Showers", "🌧️"], 82: ["Storm showers", "⛈️"], 95: ["Thunderstorm", "⛈️"] };

async function renderWeather(node, item, size) {
  const key = `qivo.wx.${item.lat},${item.lon},${item.units || "c"}`;
  let cached = JSON.parse(localStorage.getItem(key) || "null");
  const draw = (w) => {
    const [label, icon] = WMO[w.code] || ["", "🌡️"];
    node.innerHTML = `<div style="font-size:${Math.round(size * 0.35)}px;opacity:.85">${esc(item.label || "")}</div>
      <div class="temp" style="font-size:${size}px">${icon} ${Math.round(w.temp)}°${item.units === "f" ? "F" : "C"}</div>
      <div style="font-size:${Math.round(size * 0.3)}px;opacity:.8">${esc(label)} · ${Math.round(w.min)}° / ${Math.round(w.max)}°</div>`;
  };
  if (cached) draw(cached.w);
  if (!cached || Date.now() - cached.at > 30 * 60 * 1000) {
    try {
      const u = `https://api.open-meteo.com/v1/forecast?latitude=${item.lat}&longitude=${item.lon}&current=temperature_2m,weather_code&daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1${item.units === "f" ? "&temperature_unit=fahrenheit" : ""}`;
      const r = await (await fetch(u)).json();
      const w = { temp: r.current.temperature_2m, code: r.current.weather_code, max: r.daily.temperature_2m_max[0], min: r.daily.temperature_2m_min[0] };
      localStorage.setItem(key, JSON.stringify({ at: Date.now(), w }));
      draw(w);
    } catch { if (!cached) node.textContent = "Weather unavailable"; }
  }
}

// ---------------- region & layout engine ----------------
class RegionPlayer {
  constructor(region, node, layoutId, data) {
    this.region = region; this.node = node; this.layoutId = layoutId; this.data = data;
    this.idx = -1; this.alive = true; this.currentSlot = null; this.timer = null; this.order = null;
    const pl = manifest.playlists?.[region.playlist_id];
    this.items = pl ? pl.items : [];
    this.shuffle = pl?.shuffle;
  }
  cycleSeconds() {
    return this.items.reduce((s, it) => {
      if (it.type === "media" && manifest.media?.[it.media_id]?.kind === "video" && it.play_full !== false) {
        return s + (manifest.media[it.media_id].duration_ms ? manifest.media[it.media_id].duration_ms / 1000 : it.duration);
      }
      return s + it.duration;
    }, 0);
  }
  start() { this.next(); }
  stop() {
    this.alive = false;
    clearTimeout(this.timer);
    this.endPop();
    if (this.currentSlot?.cleanup) this.currentSlot.cleanup();
  }
  endPop() {
    if (this.playing) {
      const it = this.playing;
      popRecord({ layout_id: this.layoutId, media_id: it.media_id || null, item_type: it.type, started_at: it._start, duration_ms: Date.now() - it._startMs });
      this.playing = null;
    }
  }
  async next() {
    if (!this.alive) return;
    this.endPop();
    const playable = this.items.filter((it) => itemPlayable(it));
    if (!playable.length) { this.swap(null); this.timer = setTimeout(() => this.next(), 15000); return; }
    if (this.shuffle && (!this.order || this.idx + 1 >= this.order.length)) {
      this.order = [...playable].sort(() => Math.random() - 0.5); this.idx = -1;
    }
    const list = this.shuffle ? this.order : playable;
    this.idx = (this.idx + 1) % list.length;
    const item = list[this.idx];
    const r = renderItem(item, { data: this.data });
    if (!r) { this.timer = setTimeout(() => this.next(), 500); return; }
    this.swap(r);
    this.playing = Object.assign({}, item, { _start: new Date().toISOString(), _startMs: Date.now() });
    const durMs = item.duration * 1000;
    if (r.ended) {
      const cap = Math.max(durMs, (manifest.media?.[item.media_id]?.duration_ms || 3600000) + 2000);
      await Promise.race([r.ended, sleep(cap)]);
      if (this.alive) this.next();
    } else if (list.length > 1 || this.items.length > 1) {
      this.timer = setTimeout(() => this.next(), durMs);
    } else {
      // single item: keep on screen, just re-log proof-of-play each duration
      this.timer = setTimeout(() => this.relog(item), durMs);
    }
  }
  relog(item) {
    if (!this.alive) return;
    this.endPop();
    this.playing = Object.assign({}, item, { _start: new Date().toISOString(), _startMs: Date.now() });
    this.timer = setTimeout(() => this.relog(item), item.duration * 1000);
  }
  swap(r) {
    const tr = this.region.transition || "fade";
    const old = this.currentSlot;
    if (!r) { if (old) { old.cleanup(); old.el.remove(); } this.currentSlot = null; return; }
    const slot = el("div", `slot ${tr !== "none" ? "tr-" + tr : ""}`);
    slot.appendChild(r.node);
    this.node.appendChild(slot);
    const cleanup = () => { if (r.timer) clearInterval(r.timer); const v = slot.querySelector("video,audio"); if (v) { v.pause(); v.removeAttribute("src"); v.load(); } };
    this.currentSlot = { el: slot, cleanup };
    if (old) {
      if (tr === "none") { old.cleanup(); old.el.remove(); }
      else { old.el.classList.add("out-" + tr); setTimeout(() => { old.cleanup(); old.el.remove(); }, 850); }
    }
  }
}

function buildStage(layout, data, zIndex = 1) {
  const stage = el("div", "stage");
  Object.assign(stage.style, { width: layout.width + "px", height: layout.height + "px", background: layout.background || "#000", zIndex });
  const players = [];
  for (const r of [...layout.regions].sort((a, b) => (a.z ?? 0) - (b.z ?? 0))) {
    const rn = el("div", "region");
    Object.assign(rn.style, { left: r.x + "%", top: r.y + "%", width: r.w + "%", height: r.h + "%", zIndex: r.z ?? 0, background: r.background || "transparent" });
    stage.appendChild(rn);
    if (r.playlist_id) players.push(new RegionPlayer(r, rn, layout.id, data));
  }
  stage._players = players;
  viewport.appendChild(stage);
  fitStage(stage);
  players.forEach((p) => p.start());
  return stage;
}

function destroyStage(stage) {
  if (!stage) return;
  stage._players.forEach((p) => p.stop());
  stage.style.transition = "opacity .6s"; stage.style.opacity = "0";
  setTimeout(() => stage.remove(), 650);
}

function fitStage(stage) {
  const w = parseFloat(stage.style.width), h = parseFloat(stage.style.height);
  const s = Math.min(innerWidth / w, innerHeight / h);
  stage.style.transform = `scale(${s}) translate(-50%, -50%)`;
}
addEventListener("resize", () => document.querySelectorAll(".stage").forEach(fitStage));

function layoutCycleMs(layoutId) {
  const l = manifest.layouts[layoutId];
  let max = 10;
  for (const r of l.regions) {
    const pl = manifest.playlists?.[r.playlist_id];
    if (!pl) continue;
    const rp = new RegionPlayer(r, null, layoutId);
    max = Math.max(max, rp.cycleSeconds());
  }
  return max * 1000;
}

function showLayout(id) {
  const old = current.stage;
  current.stage = buildStage(manifest.layouts[id], {});
  current.layoutId = id;
  current.until = Date.now() + layoutCycleMs(id);
  destroyStage(old);
  reportStatus();
}

/** Main loop: evaluate schedule, rotate equal-priority layouts, handle interrupts. */
function tick() {
  if (!manifest || manifest.status !== "active") return;
  if (interrupt && Date.now() >= interrupt.until) {
    destroyStage(interrupt.stage); interrupt = null; reportStatus();
  }
  const { layouts } = activeLayouts(manifest);
  const key = layouts.join(",");
  if (!layouts.length) {
    if (current.stage) { destroyStage(current.stage); current.stage = null; current.key = ""; }
    sysScreen(`<p>No content scheduled for <b>${esc(manifest.display?.name)}</b>.</p><p class="small">Set a default layout or a schedule in the dashboard.</p>`);
    return;
  }
  sysScreen(null);
  if (key !== current.key) {
    current.key = key; current.layouts = layouts; current.idx = 0;
    showLayout(layouts[0]);
  } else if (layouts.length > 1 && Date.now() >= current.until) {
    current.idx = (current.idx + 1) % layouts.length;
    showLayout(layouts[current.idx]);
  }
}

function applyManifest(m, force = false) {
  const changed = !manifest || manifest.version !== m.version;
  manifest = m;
  if (!PREVIEW) localStorage.setItem(LS.manifest, JSON.stringify(m));
  if (changed || force) {
    current.key = ""; // force rebuild with the new content
    syncMediaCache(m).catch(() => {});
  }
  tick();
}

// ---------------- network ----------------
async function loadManifest() {
  try {
    const m = PREVIEW ? await api(`/api/preview/layout/${encodeURIComponent(PREVIEW)}`) : await api("/api/player/manifest");
    if (m.status === "pending") { manifest = null; showPairing(m.pairing_code); return; }
    if (m.status === "disabled") { manifest = null; sysScreen(`<p>This screen has been disabled by an administrator.</p>`); return; }
    if (m.status === "offline") throw new Error("offline");
    applyManifest(m);
  } catch (e) {
    if (e.status === 401 && !PREVIEW) { // key revoked or display deleted → re-register
      localStorage.removeItem(LS.key); localStorage.removeItem(LS.manifest);
      return boot();
    }
    const cached = JSON.parse(localStorage.getItem(LS.manifest) || "null");
    if (cached && !manifest) applyManifest(cached); // offline start
  }
}

function connect() {
  if (PREVIEW || !deviceKey()) return;
  const proto = location.protocol === "https:" ? "wss" : "ws";
  ws = new WebSocket(`${proto}://${location.host}/ws/player?key=${encodeURIComponent(deviceKey())}`);
  ws.onopen = () => { wsBackoff = 1000; loadManifest(); flushPop(); };
  ws.onmessage = (ev) => {
    let msg; try { msg = JSON.parse(ev.data); } catch { return; }
    switch (msg.type) {
      case "claimed": ws.close(); loadManifest(); break; // reconnect with org scope
      case "refresh": loadManifest(); break;
      case "unpaired": localStorage.clear(); location.reload(); break;
      case "interrupt": startInterrupt(msg); break;
      case "command": runCommand(msg.command); break;
    }
  };
  ws.onclose = () => { setTimeout(connect, wsBackoff); wsBackoff = Math.min(wsBackoff * 2, 30000); };
  ws.onerror = () => ws.close();
}

function startInterrupt(msg) {
  const layout = manifest?.layouts?.[msg.layout_id];
  if (!layout) { loadManifest(); return; }
  if (interrupt) destroyStage(interrupt.stage);
  const stage = buildStage(layout, msg.data || {}, 10);
  interrupt = { stage, until: Date.now() + msg.duration_s * 1000, layout_id: msg.layout_id };
  reportStatus();
}

async function runCommand(cmd) {
  if (cmd === "reload") location.reload();
  if (cmd === "refresh") loadManifest();
  if (cmd === "identify") overlay(`<b>${esc(manifest?.display?.name || "Unpaired screen")}</b>${screen.width}×${screen.height} · v${esc(manifest?.version || "-")} · ${navigator.onLine ? "online" : "offline"}`, 10000);
  if (cmd === "clear_cache") {
    for (const k of await caches.keys()) await caches.delete(k);
    location.reload();
  }
}

function reportStatus() {
  if (!ws || ws.readyState !== 1) return;
  ws.send(JSON.stringify({
    type: "status",
    layout_id: interrupt?.layout_id || current.layoutId || null,
    version: manifest?.version || null,
    info: {
      user_agent: navigator.userAgent, screen: `${screen.width}x${screen.height}`, viewport: `${innerWidth}x${innerHeight}`,
      uptime_s: Math.round((Date.now() - startedAt) / 1000), pop_queue: JSON.parse(localStorage.getItem(LS.pop) || "[]").length,
      language: navigator.language, timezone: Intl.DateTimeFormat().resolvedOptions().timeZone, player: "web/0.1",
    },
  }));
}

// ---------------- boot ----------------
async function boot() {
  if (PREVIEW) {
    document.body.style.cursor = "default";
    document.body.appendChild(el("div", "preview-badge", "Preview"));
    await loadManifest();
    setInterval(tick, 1000);
    return;
  }
  if ("serviceWorker" in navigator) navigator.serviceWorker.register("/player/sw.js", { scope: "/player/" }).catch(() => {});
  if (!deviceKey()) {
    sysScreen(`<p>Registering screen…</p>`);
    for (;;) {
      try {
        const r = await api("/api/player/register", { method: "POST", body: JSON.stringify({ info: { user_agent: navigator.userAgent, screen: `${screen.width}x${screen.height}` } }) });
        localStorage.setItem(LS.key, r.device_key); localStorage.setItem(LS.id, r.display_id);
        break;
      } catch { await sleep(5000); }
    }
  }
  const cached = JSON.parse(localStorage.getItem(LS.manifest) || "null");
  if (cached) applyManifest(cached);
  else sysScreen(`<p>Loading content…</p><div class="progress"><div></div></div>`);
  await loadManifest();
  connect();
  setInterval(tick, 1000);
  setInterval(flushPop, 60000);
  setInterval(reportStatus, 60000);
  setInterval(() => { if (!ws || ws.readyState !== 1) loadManifest(); }, 60000); // poll fallback
  try { await navigator.wakeLock?.request("screen"); } catch {}
  document.addEventListener("dblclick", () => document.documentElement.requestFullscreen?.().catch(() => {}));
}

boot();
