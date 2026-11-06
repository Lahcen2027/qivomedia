import { api, h, esc, fmtDur, modal, confirmDialog, guard, formData, load, invalidate, can, toast } from "../core.js";
import { pickMedia } from "./media.js";

const WIDGETS = {
  text: { icon: "T", label: "Text", def: { text: "Your message here", size: 96, color: "#ffffff", bg: "#000000", align: "center", bold: true } },
  ticker: { icon: "⇆", label: "Ticker", def: { text: "Breaking: Qivomedia is live on every screen  •  ", size: 54, color: "#ffffff", bg: "#7c5cff", speed: 140 } },
  clock: { icon: "🕒", label: "Clock", def: { size: 140, color: "#ffffff", bg: "transparent", format: "24h", show_date: true, seconds: false } },
  weather: { icon: "⛅", label: "Weather", def: { lat: 33.57, lon: -7.59, label: "Casablanca", units: "c", size: 120, color: "#ffffff", bg: "transparent" } },
  countdown: { icon: "⏳", label: "Countdown", def: { target: "", label: "Grand opening in", size: 120, color: "#ffffff", bg: "transparent" } },
  webpage: { icon: "🌐", label: "Web page", def: { url: "https://", zoom: 1 } },
  html: { icon: "</>", label: "HTML", def: { html: "<div style='color:#fff;font:700 80px system-ui;display:grid;place-items:center;height:100%'>Hello {{name}}</div>" } },
};

const totalSecs = (items, mediaById = {}) =>
  items.reduce((s, it) => s + (it.type === "media" && mediaById[it.media_id]?.kind === "video" && it.play_full !== false && mediaById[it.media_id]?.duration_ms ? mediaById[it.media_id].duration_ms / 1000 : it.duration), 0);

export async function render(root) {
  invalidate("playlists");
  const [pls, media] = await Promise.all([load("playlists"), load("media")]);
  const byId = Object.fromEntries(media.map((m) => [m.id, m]));
  const page = h(`<div>
    <div class="head"><div><h1>Playlists</h1><p>Ordered sequences of media and live widgets. Assign them to layout zones.</p></div>
      ${can.edit() ? `<button class="btn primary" data-new>+ New playlist</button>` : ""}</div>
    <div class="grid g3">${pls.length ? pls.map((p) => `<div class="card" data-id="${p.id}">
      <div class="row"><h2 class="grow">${esc(p.name)}</h2>${p.shuffle ? `<span class="badge">shuffle</span>` : ""}</div>
      <p class="muted small" style="margin:6px 0 14px">${p.items.length} item${p.items.length === 1 ? "" : "s"} · ${fmtDur(totalSecs(p.items, byId))} loop</p>
      <div class="row"><a class="btn sm" href="#/playlists/${p.id}">Edit</a>${can.edit() ? `<button class="btn sm" data-dup>Duplicate</button><button class="btn sm ghost danger" data-del>Delete</button>` : ""}</div>
    </div>`).join("") : `<div class="card empty" style="grid-column:1/-1"><h3>No playlists yet</h3><p>Create one, then add media and widgets.</p></div>`}</div>
  </div>`);
  root.appendChild(page);
  page.querySelector("[data-new]")?.addEventListener("click", async () => {
    const r = await modal({ title: "New playlist", submit: "Create", body: `<div class="field"><label>Name</label><input name="name" required placeholder="Morning menu"></div>`,
      onSubmit: (f) => api("/playlists", { method: "POST", body: formData(f) }) });
    if (r?.id) location.hash = `#/playlists/${r.id}`;
  });
  page.querySelectorAll("[data-id]").forEach((c) => {
    const id = c.dataset.id;
    c.querySelector("[data-dup]")?.addEventListener("click", async () => { await guard(() => api(`/playlists/${id}/duplicate`, { method: "POST" }), "Duplicated"); root.innerHTML = ""; render(root); });
    c.querySelector("[data-del]")?.addEventListener("click", async () => {
      if (!(await confirmDialog("Delete playlist?", "This cannot be undone."))) return;
      await guard(() => api(`/playlists/${id}`, { method: "DELETE" }), "Deleted");
      root.innerHTML = ""; render(root);
    });
  });
}

export async function edit(root, id) {
  invalidate("media");
  const [pl, media] = await Promise.all([api(`/playlists/${id}`), load("media")]);
  const byId = Object.fromEntries(media.map((m) => [m.id, m]));
  let items = pl.items;
  let sel = items[0]?.id || null;
  let dirty = false;

  const page = h(`<div>
    <div class="head"><div><a href="#/playlists" class="small">← Playlists</a><h1 style="margin-top:4px"><input value="${esc(pl.name)}" data-name style="font:inherit;font-size:22px;font-weight:700;background:transparent;border-color:transparent;padding:2px 6px;margin-left:-6px;width:420px"></h1></div>
      <div class="row"><label class="chk"><input type="checkbox" data-shuffle ${pl.shuffle ? "checked" : ""}> Shuffle</label>
      <span class="muted small" data-total></span>${can.edit() ? `<button class="btn primary" data-save>Save</button>` : ""}</div></div>
    <div class="pl-wrap">
      <div>
        ${can.edit() ? `<div class="addbar card" style="margin-bottom:12px;padding:10px"><button class="btn sm primary" data-add="media">+ Media</button>
          ${Object.entries(WIDGETS).map(([k, w]) => `<button class="btn sm" data-add="${k}">${w.icon} ${w.label}</button>`).join("")}</div>` : ""}
        <div class="items"></div>
      </div>
      <div class="card props" style="position:sticky;top:20px"></div>
    </div></div>`);
  root.appendChild(page);
  const list = page.querySelector(".items");
  const props = page.querySelector(".props");
  const mark = () => { dirty = true; page.querySelector("[data-save]")?.classList.add("primary"); updateTotal(); };
  const updateTotal = () => page.querySelector("[data-total]").textContent = `${items.length} items · ${fmtDur(totalSecs(items, byId))} loop`;

  const label = (it) => it.type === "media" ? (byId[it.media_id]?.name || "Missing media") : (WIDGETS[it.type]?.label || it.type) + (it.text ? ` — ${it.text.slice(0, 40)}` : it.url ? ` — ${it.url}` : it.label ? ` — ${it.label}` : "");
  const thumbOf = (it) => {
    if (it.type !== "media") return `<div class="th" style="background:${esc(it.bg && it.bg !== "transparent" ? it.bg : "#1a2030")}">${WIDGETS[it.type]?.icon || "?"}</div>`;
    const m = byId[it.media_id];
    if (m?.kind === "image") return `<div class="th"><img src="${m.url}"></div>`;
    if (m?.kind === "video") return `<div class="th"><video src="${m.url}#t=1" muted preload="metadata"></video></div>`;
    return `<div class="th">📄</div>`;
  };

  function drawList() {
    list.innerHTML = items.length ? "" : `<div class="card empty"><h3>Empty playlist</h3><p>Add media or widgets above.</p></div>`;
    items.forEach((it, i) => {
      const row = h(`<div class="item ${it.id === sel ? "sel" : ""}" draggable="${can.edit()}" data-i="${i}">
        <span class="grip">⋮⋮</span>${thumbOf(it)}
        <div style="min-width:0"><div style="font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${esc(label(it))}</div>
        <div class="small muted">${it.type === "media" ? esc(byId[it.media_id]?.kind || "") : "widget"} · ${it.type === "media" && byId[it.media_id]?.kind === "video" && it.play_full !== false ? "full length" : it.duration + "s"}${it.when ? " · day-parted" : ""}</div></div>
        ${can.edit() ? `<button class="btn sm ghost" data-rm title="Remove">✕</button>` : ""}</div>`);
      row.onclick = (e) => { if (e.target.closest("[data-rm]")) return; sel = it.id; drawList(); drawProps(); };
      row.querySelector("[data-rm]")?.addEventListener("click", () => { items.splice(i, 1); if (sel === it.id) sel = items[0]?.id; mark(); drawList(); drawProps(); });
      row.ondragstart = (e) => { e.dataTransfer.setData("text/plain", i); row.classList.add("dragging"); };
      row.ondragend = () => row.classList.remove("dragging");
      row.ondragover = (e) => e.preventDefault();
      row.ondrop = (e) => {
        e.preventDefault();
        const from = +e.dataTransfer.getData("text/plain");
        const [moved] = items.splice(from, 1);
        items.splice(i, 0, moved);
        mark(); drawList();
      };
      list.appendChild(row);
    });
  }

  function field(key, lab, type = "text", extra = "") {
    const it = items.find((x) => x.id === sel);
    const DEF_TRUE = ["show_date", "play_full", "mute"];
    const v = it[key] ?? (DEF_TRUE.includes(key) ? true : type === "color" ? "#ffffff" : "");
    if (type === "checkbox") return `<div class="field"><label class="chk"><input type="checkbox" data-k="${key}" ${v ? "checked" : ""}> ${lab}</label></div>`;
    if (type === "textarea") return `<div class="field"><label>${lab}</label><textarea data-k="${key}" ${extra}>${esc(v)}</textarea></div>`;
    if (type.startsWith("select:")) return `<div class="field"><label>${lab}</label><select data-k="${key}">${type.slice(7).split(",").map((o) => `<option ${o === String(v) ? "selected" : ""}>${o}</option>`).join("")}</select></div>`;
    return `<div class="field"><label>${lab}</label><input type="${type}" data-k="${key}" value="${esc(v)}" ${extra}></div>`;
  }

  function drawProps() {
    const it = items.find((x) => x.id === sel);
    if (!it) { props.innerHTML = `<div class="muted">Select an item to edit its settings.</div>`; return; }
    const isVideo = it.type === "media" && byId[it.media_id]?.kind === "video";
    const w = it.when || {};
    let f = `<h2 style="margin-bottom:14px">${esc(it.type === "media" ? "Media item" : WIDGETS[it.type]?.label)}</h2>`;
    f += field("duration", isVideo && it.play_full !== false ? "Duration (s) — used if video can't report its length" : "Duration (seconds)", "number", 'min="0.5" step="0.5"');
    if (it.type === "media") {
      f += field("fit", "Fit", "select:contain,cover,fill");
      if (isVideo) f += field("play_full", "Play full video length", "checkbox") + field("mute", "Muted", "checkbox");
    }
    if (["text", "ticker"].includes(it.type)) f += field("text", "Text — use {{placeholders}} for trigger data", "textarea");
    if (it.type === "text") f += `<div class="fields2">${field("align", "Align", "select:left,center,right")}${field("bold", "Bold", "checkbox")}</div>`;
    if (it.type === "ticker") f += field("speed", "Speed (px/s)", "number");
    if (it.type === "clock") f += `<div class="fields2">${field("format", "Format", "select:24h,12h")}${field("locale", "Locale (e.g. fr-MA)")}</div>${field("show_date", "Show date", "checkbox")}${field("seconds", "Show seconds", "checkbox")}`;
    if (it.type === "weather") f += `<div class="fields2">${field("lat", "Latitude", "number", 'step="any"')}${field("lon", "Longitude", "number", 'step="any"')}</div>${field("label", "Label")}${field("units", "Units", "select:c,f")}`;
    if (it.type === "countdown") f += field("target", "Target date & time", "datetime-local") + field("label", "Label");
    if (it.type === "webpage") f += field("url", "URL", "url") + field("zoom", "Zoom", "number", 'step="0.05" min="0.25" max="4"');
    if (it.type === "html") f += field("html", "HTML (sandboxed) — {{placeholders}} supported", "textarea", 'style="min-height:160px;font-family:ui-monospace,monospace;font-size:12px"');
    if (!["media", "webpage", "html"].includes(it.type)) f += `<div class="fields3">${field("size", "Font size (px)", "number")}${field("color", "Color", "color")}${field("bg", "Background", "text")}</div>`;
    const days = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
    f += `<details ${it.when ? "open" : ""} style="margin-top:6px"><summary class="muted" style="cursor:pointer;margin-bottom:10px">Day-parting (only play this item at certain times)</summary>
      <div class="field"><label class="chk"><input type="checkbox" data-when-on ${it.when ? "checked" : ""}> Restrict when this item plays</label></div>
      <div class="days field">${days.map((d, i) => `<label><input type="checkbox" data-day="${i}" ${((w.days_mask ?? 127) >> i) & 1 ? "checked" : ""}><span>${d}</span></label>`).join("")}</div>
      <div class="fields2"><div class="field"><label>From</label><input type="time" data-w="time_from" value="${esc(w.time_from || "")}"></div><div class="field"><label>To</label><input type="time" data-w="time_to" value="${esc(w.time_to || "")}"></div></div>
      <div class="fields2"><div class="field"><label>Valid from</label><input type="datetime-local" data-w="valid_from" value="${esc(w.valid_from || "")}"></div><div class="field"><label>Valid until</label><input type="datetime-local" data-w="valid_to" value="${esc(w.valid_to || "")}"></div></div>
    </details>`;
    props.innerHTML = f;
    if (!can.edit()) props.querySelectorAll("input,select,textarea").forEach((x) => (x.disabled = true));

    props.querySelectorAll("[data-k]").forEach((inp) => inp.oninput = inp.onchange = () => {
      const k = inp.dataset.k;
      it[k] = inp.type === "checkbox" ? inp.checked : inp.type === "number" ? parseFloat(inp.value) : inp.value;
      mark();
      const row = list.querySelector(`.item.sel`);
      if (row && (k === "duration" || k === "text" || k === "play_full" || k === "label" || k === "url")) drawList();
    });
    const syncWhen = () => {
      if (!props.querySelector("[data-when-on]").checked) { delete it.when; mark(); drawList(); return; }
      let mask = 0;
      props.querySelectorAll("[data-day]").forEach((d) => { if (d.checked) mask |= 1 << +d.dataset.day; });
      it.when = { days_mask: mask || 127 };
      props.querySelectorAll("[data-w]").forEach((x) => { if (x.value) it.when[x.dataset.w] = x.value; });
      mark(); drawList();
    };
    props.querySelectorAll("[data-when-on],[data-day],[data-w]").forEach((x) => x.onchange = syncWhen);
  }

  page.querySelectorAll("[data-add]").forEach((b) => b.onclick = async () => {
    const t = b.dataset.add;
    if (t === "media") {
      const picked = await pickMedia();
      for (const m of picked) {
        const it = { id: crypto.randomUUID(), type: "media", media_id: m.id, duration: m.kind === "video" && m.duration_ms ? Math.ceil(m.duration_ms / 1000) : 10, fit: "contain", play_full: true, mute: true };
        byId[m.id] = m; items.push(it); sel = it.id;
      }
      if (picked.length) mark();
    } else {
      const it = { id: crypto.randomUUID(), type: t, duration: t === "ticker" ? 30 : 15, ...structuredClone(WIDGETS[t].def) };
      items.push(it); sel = it.id; mark();
    }
    drawList(); drawProps();
  });

  page.querySelector("[data-name]").oninput = mark;
  page.querySelector("[data-shuffle]").onchange = mark;
  page.querySelector("[data-save]")?.addEventListener("click", async () => {
    const r = await guard(() => api(`/playlists/${id}`, { method: "PUT", body: { name: page.querySelector("[data-name]").value, shuffle: page.querySelector("[data-shuffle]").checked, items } }), "Playlist saved — screens update instantly");
    items = r.items; dirty = false; invalidate("playlists"); drawList();
  });
  window.onbeforeunload = () => (dirty && root.isConnected ? "unsaved" : undefined);
  updateTotal(); drawList(); drawProps();
}
