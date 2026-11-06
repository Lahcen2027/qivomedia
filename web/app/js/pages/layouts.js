import { api, h, esc, modal, confirmDialog, guard, formData, load, invalidate, can, store, toast, ICON } from "../core.js";

const TEMPLATES = {
  "Full screen": [{ name: "Main", x: 0, y: 0, w: 100, h: 100 }],
  "Main + ticker": [{ name: "Main", x: 0, y: 0, w: 100, h: 88 }, { name: "Ticker", x: 0, y: 88, w: 100, h: 12 }],
  "L-bar": [{ name: "Main", x: 0, y: 0, w: 75, h: 85 }, { name: "Side", x: 75, y: 0, w: 25, h: 85 }, { name: "Bottom", x: 0, y: 85, w: 100, h: 15 }],
  "Split 50/50": [{ name: "Left", x: 0, y: 0, w: 50, h: 100 }, { name: "Right", x: 50, y: 0, w: 50, h: 100 }],
  "Quad": [{ name: "A", x: 0, y: 0, w: 50, h: 50 }, { name: "B", x: 50, y: 0, w: 50, h: 50 }, { name: "C", x: 0, y: 50, w: 50, h: 50 }, { name: "D", x: 50, y: 50, w: 50, h: 50 }],
  "Header + main + clock": [{ name: "Header", x: 0, y: 0, w: 80, h: 14 }, { name: "Clock", x: 80, y: 0, w: 20, h: 14 }, { name: "Main", x: 0, y: 14, w: 100, h: 86 }],
};
const RES = { "1920×1080 (Full HD landscape)": [1920, 1080], "1080×1920 (portrait)": [1080, 1920], "3840×2160 (4K)": [3840, 2160], "1280×720 (HD)": [1280, 720], "3840×1080 (2×1 video wall)": [3840, 1080] };

const tplPreview = (regions) => regions.map((r) => `<i style="left:${r.x}%;top:${r.y}%;width:${r.w}%;height:${r.h}%"></i>`).join("");

export async function render(root) {
  invalidate("layouts");
  const layouts = await load("layouts");
  const page = h(`<div>
    <div class="head"><div><h1>Layouts</h1><p>Multi-zone screen designs. Each zone plays its own playlist with transitions.</p></div>
      <div class="row">${can.edit() && store.me.ai?.available ? `<button class="btn ai-btn" data-ai>${ICON.sparkle}<span>Create with AI</span></button>` : ""}${can.edit() ? `<button class="btn primary" data-new>+ New layout</button>` : ""}</div></div>
    <div class="grid g3">${layouts.length ? layouts.map((l) => `<div class="card" data-id="${l.id}">
      <div class="layout-thumb" style="background:${esc(l.background)};aspect-ratio:${l.width}/${l.height};max-height:180px;margin:0 auto 12px">${tplPreview(l.regions)}</div>
      <div class="row"><h2 class="grow">${esc(l.name)}</h2>${l.origin === "ai" ? `<span class="badge acc" title="Designed by the AI agent">AI</span>` : ""}<span class="badge">${l.width}×${l.height}</span></div>
      <p class="small muted" style="margin:4px 0 12px">${l.regions.length} zone${l.regions.length === 1 ? "" : "s"}</p>
      <div class="row"><a class="btn sm" href="#/layouts/${l.id}">Design</a><a class="btn sm" href="/player/?preview=${l.id}" target="_blank">Preview ↗</a>
      ${can.edit() ? `<button class="btn sm" data-dup>Duplicate</button><button class="btn sm ghost danger" data-del>Delete</button>` : ""}</div></div>`).join("")
      : `<div class="card empty" style="grid-column:1/-1"><h3>No layouts yet</h3><p>Start from a template — full screen, L-bar, split, quad…</p></div>`}</div></div>`);
  root.appendChild(page);

  page.querySelector("[data-ai]")?.addEventListener("click", () => aiCreate());
  page.querySelector("[data-new]")?.addEventListener("click", async () => {
    let tpl = "Full screen";
    const r = await modal({
      title: "New layout", submit: "Create", wide: true,
      body: `<div class="fields2"><div class="field"><label>Name</label><input name="name" required placeholder="Lobby — morning"></div>
        <div class="field"><label>Resolution</label><select name="res">${Object.keys(RES).map((k) => `<option>${k}</option>`).join("")}</select></div></div>
        <label>Template</label><div class="tpl">${Object.entries(TEMPLATES).map(([k, rs]) => `<div><button type="button" data-t="${k}" style="${k === tpl ? "border-color:var(--accent)" : ""}">${tplPreview(rs)}</button><div class="small muted" style="text-align:center;margin-top:4px">${k}</div></div>`).join("")}</div>`,
      onOpen: (f) => f.querySelectorAll("[data-t]").forEach((b) => b.onclick = () => {
        tpl = b.dataset.t; f.querySelectorAll("[data-t]").forEach((x) => x.style.borderColor = ""); b.style.borderColor = "var(--accent)";
      }),
      onSubmit: (f) => {
        const v = formData(f);
        const [width, height] = RES[v.res];
        return api("/layouts", { method: "POST", body: { name: v.name, width, height, background: "#000000", regions: TEMPLATES[tpl].map((r, i) => ({ ...r, z: i, transition: "fade" })) } });
      },
    });
    if (r?.id) location.hash = `#/layouts/${r.id}`;
  });
  page.querySelectorAll("[data-id]").forEach((c) => {
    const id = c.dataset.id;
    c.querySelector("[data-dup]")?.addEventListener("click", async () => { await guard(() => api(`/layouts/${id}/duplicate`, { method: "POST" }), "Duplicated"); root.innerHTML = ""; render(root); });
    c.querySelector("[data-del]")?.addEventListener("click", async () => {
      if (!(await confirmDialog("Delete layout?", "Schedules and triggers using it will be removed too."))) return;
      await guard(() => api(`/layouts/${id}`, { method: "DELETE" }), "Deleted");
      root.innerHTML = ""; render(root);
    });
  });
}

export async function edit(root, id) {
  invalidate("playlists");
  const [layout, playlists] = await Promise.all([api(`/layouts/${id}`), load("playlists")]);
  let regions = layout.regions;
  let sel = regions[0]?.id;
  const page = h(`<div>
    <div class="head"><div><a href="#/layouts" class="small">← Layouts</a><h1 style="margin-top:4px">${esc(layout.name)}</h1></div>
      <div class="row">${store.me.user.platform_admin ? `<button class="btn" data-web title="Show this layout in the live player on your public website">Show on website</button>` : ""}<a class="btn" target="_blank" data-prev>Preview ↗</a>${can.edit() ? `<button class="btn" data-addz>+ Zone</button><button class="btn primary" data-save>Save</button>` : ""}</div></div>
    <div class="ai-note"></div>
    ${can.edit() && store.me.ai?.available ? `<form class="ai-bar card" data-aibar><span class="ai-ico">${ICON.sparkle}</span><input name="q" placeholder="Ask AI to change this layout — e.g. “make the headline bigger and add the weather for Rabat”" autocomplete="off"><button class="btn ai-btn" type="submit">Change</button></form>` : ""}
    <div class="designer">
      <div class="canvas-wrap"><div class="canvas"></div></div>
      <div class="stack">
        <div class="card zprops"></div>
        <div class="card">
          <h2 style="margin-bottom:12px">Layout</h2>
          <div class="field"><label>Name</label><input data-l="name" value="${esc(layout.name)}"></div>
          <div class="fields3"><div class="field"><label>Width</label><input type="number" data-l="width" value="${layout.width}"></div>
          <div class="field"><label>Height</label><input type="number" data-l="height" value="${layout.height}"></div>
          <div class="field"><label>Background</label><input type="color" data-l="background" value="${esc(layout.background)}"></div></div>
          <label>Apply template</label><div class="tpl">${Object.entries(TEMPLATES).map(([k, rs]) => `<button type="button" title="${k}" data-t="${k}">${tplPreview(rs)}</button>`).join("")}</div>
        </div>
      </div>
    </div></div>`);
  root.appendChild(page);
  const canvas = page.querySelector(".canvas");
  const zprops = page.querySelector(".zprops");
  const webBtn = page.querySelector("[data-web]");
  if (webBtn) {
    const setLabel = (on) => { webBtn.textContent = on ? "✓ On website" : "Show on website"; webBtn.classList.toggle("ai-btn", on); };
    api("/platform/settings/demo-layout").then((r) => setLabel(r.layout_id === id)).catch(() => {});
    webBtn.onclick = async () => {
      const cur = await api("/platform/settings/demo-layout");
      const on = cur.layout_id !== id;
      await guard(() => api("/platform/settings/demo-layout", { method: "PUT", body: { layout_id: on ? id : null } }),
        on ? "This layout now plays in your website's hero" : "Removed from website — the built-in demo plays instead");
      setLabel(on);
    };
  }
  page.querySelector("[data-prev]").href = `/player/?preview=${id}`;

  const plName = (pid) => playlists.find((p) => p.id === pid)?.name;
  let scale = 1;
  function sizeCanvas() {
    const wrap = page.querySelector(".canvas-wrap");
    const maxW = wrap.clientWidth - 48, maxH = Math.min(window.innerHeight - 220, 720);
    scale = Math.min(maxW / layout.width, maxH / layout.height);
    Object.assign(canvas.style, { width: layout.width * scale + "px", height: layout.height * scale + "px", background: layout.background });
  }

  function drawZones() {
    canvas.innerHTML = "";
    regions.forEach((r, i) => {
      const z = h(`<div class="zone ${r.id === sel ? "sel" : ""}" style="left:${r.x}%;top:${r.y}%;width:${r.w}%;height:${r.h}%;z-index:${r.z ?? i}">
        <div>${esc(r.name || "Zone " + (i + 1))}</div><div class="sub">${esc(plName(r.playlist_id) || "no playlist")}</div>${can.edit() ? `<div class="h"></div>` : ""}</div>`);
      z.onmousedown = (e) => { sel = r.id; drawProps(); canvas.querySelectorAll(".zone").forEach((x) => x.classList.remove("sel")); z.classList.add("sel"); if (can.edit()) drag(e, r, z, e.target.classList.contains("h")); };
      canvas.appendChild(z);
    });
  }

  function drag(e, r, z, resize) {
    e.preventDefault();
    const sx = e.clientX, sy = e.clientY, o = { ...r };
    const cw = canvas.clientWidth, ch = canvas.clientHeight;
    const snap = (v) => Math.round(v * 2) / 2; // 0.5% grid
    const move = (ev) => {
      const dx = ((ev.clientX - sx) / cw) * 100, dy = ((ev.clientY - sy) / ch) * 100;
      if (resize) { r.w = snap(Math.max(2, Math.min(100 - r.x, o.w + dx))); r.h = snap(Math.max(2, Math.min(100 - r.y, o.h + dy))); }
      else { r.x = snap(Math.max(0, Math.min(100 - r.w, o.x + dx))); r.y = snap(Math.max(0, Math.min(100 - r.h, o.y + dy))); }
      Object.assign(z.style, { left: r.x + "%", top: r.y + "%", width: r.w + "%", height: r.h + "%" });
      drawProps(true);
    };
    const up = () => { removeEventListener("mousemove", move); removeEventListener("mouseup", up); };
    addEventListener("mousemove", move); addEventListener("mouseup", up);
  }

  function drawProps(numbersOnly = false) {
    const r = regions.find((x) => x.id === sel);
    if (numbersOnly && r) { ["x", "y", "w", "h"].forEach((k) => { const i = zprops.querySelector(`[data-z=${k}]`); if (i) i.value = r[k]; }); return; }
    if (!r) { zprops.innerHTML = `<div class="muted">Select a zone on the canvas.</div>`; return; }
    const px = (k, total) => Math.round((r[k] / 100) * total);
    zprops.innerHTML = `<div class="row" style="margin-bottom:12px"><h2 class="grow">Zone</h2>${can.edit() ? `<button class="btn sm danger" data-rmz>Delete zone</button>` : ""}</div>
      <div class="field"><label>Name</label><input data-z="name" value="${esc(r.name || "")}"></div>
      <div class="field"><label>Playlist</label><select data-z="playlist_id"><option value="">— none —</option>${playlists.map((p) => `<option value="${p.id}" ${p.id === r.playlist_id ? "selected" : ""}>${esc(p.name)}</option>`).join("")}</select>
        <div class="small" style="margin-top:4px">${r.playlist_id ? `<a href="#/playlists/${r.playlist_id}">Edit playlist →</a>` : `<a href="#/playlists">Create a playlist →</a>`}</div></div>
      <div class="fields2"><div class="field"><label>Transition</label><select data-z="transition">${["fade", "slide", "zoom", "none"].map((t) => `<option ${t === r.transition ? "selected" : ""}>${t}</option>`).join("")}</select></div>
      <div class="field"><label>Layer (z)</label><input type="number" data-z="z" value="${r.z ?? 0}"></div></div>
      <div class="fields2">${["x", "y", "w", "h"].map((k) => `<div class="field"><label>${{ x: "Left", y: "Top", w: "Width", h: "Height" }[k]} %</label><input type="number" step="0.5" min="0" max="100" data-z="${k}" value="${r[k]}"></div>`).join("")}</div>
      <div class="small faint">${px("w", layout.width)}×${px("h", layout.height)} px at (${px("x", layout.width)}, ${px("y", layout.height)})</div>`;
    if (!can.edit()) zprops.querySelectorAll("input,select").forEach((x) => (x.disabled = true));
    zprops.querySelectorAll("[data-z]").forEach((inp) => inp.onchange = () => {
      const k = inp.dataset.z;
      r[k] = inp.type === "number" ? parseFloat(inp.value) : inp.value;
      drawZones();
    });
    zprops.querySelector("[data-rmz]")?.addEventListener("click", () => { regions = regions.filter((x) => x.id !== r.id); sel = regions[0]?.id; drawZones(); drawProps(); });
  }

  page.querySelectorAll("[data-l]").forEach((inp) => inp.onchange = () => {
    const k = inp.dataset.l;
    layout[k] = inp.type === "number" ? parseInt(inp.value, 10) : inp.value;
    sizeCanvas();
  });
  page.querySelectorAll("[data-t]").forEach((b) => b.onclick = () => {
    if (!can.edit()) return;
    const old = regions;
    regions = TEMPLATES[b.dataset.t].map((r, i) => ({ ...r, id: crypto.randomUUID(), z: i, transition: "fade", playlist_id: old[i]?.playlist_id || "" }));
    sel = regions[0].id; drawZones(); drawProps();
  });
  page.querySelector("[data-addz]")?.addEventListener("click", () => {
    const r = { id: crypto.randomUUID(), name: `Zone ${regions.length + 1}`, x: 30, y: 30, w: 40, h: 40, z: regions.length, transition: "fade", playlist_id: "" };
    regions.push(r); sel = r.id; drawZones(); drawProps();
  });
  page.querySelector("[data-save]")?.addEventListener("click", async () => {
    const r = await guard(() => api(`/layouts/${id}`, { method: "PUT", body: { name: layout.name, width: layout.width, height: layout.height, background: layout.background, regions } }), "Layout saved — live screens updated");
    regions = r.regions; page.querySelector("h1").textContent = r.name; invalidate("layouts"); drawZones();
  });
  addEventListener("resize", () => root.isConnected && sizeCanvas());
  sizeCanvas(); drawZones(); drawProps();

  const note = store.cache.aiNote;
  if (note && note.layoutId === id) {
    delete store.cache.aiNote;
    page.querySelector(".ai-note").appendChild(aiSummaryCard(note.summary, id));
  }
  page.querySelector("[data-aibar]")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const f = e.target, q = f.q.value.trim();
    if (q.length < 3) return;
    const btn = f.querySelector("button");
    btn.disabled = true; f.q.disabled = true; btn.innerHTML = `<span class="spin"></span> Working…`;
    try {
      const r = await api("/ai/layouts", { method: "POST", body: { prompt: q, layout_id: id } });
      store.cache.aiNote = { layoutId: id, summary: r.summary };
      invalidate("layouts", "playlists");
      root.innerHTML = ""; edit(root, id);
    } catch (err) {
      toast(err.message, true);
      btn.disabled = false; f.q.disabled = false; btn.textContent = "Change";
    }
  });
}


function aiSummaryCard(summary, id) {
  const c = h(`<div class="card ai-summary"><div class="row"><span class="ai-ico">${ICON.sparkle}</span><div class="grow"><b>AI draft ready.</b> <span class="muted">${esc(summary || "")}</span></div>
    <a class="btn sm" target="_blank" href="/player/?preview=${id}">Preview ↗</a><button class="btn sm ghost" data-x>✕</button></div>
    <div class="small faint" style="margin-top:6px">It isn't on any screen yet. Edit anything you like, then assign it as a screen's default layout or add it to the schedule.</div></div>`);
  c.querySelector("[data-x]").onclick = () => c.remove();
  return c;
}

const EXAMPLES = [
  "Welcome screen for the Faculty of Sciences lobby: big welcome message, today's date and time, weather for Rabat, and a ticker with campus news",
  "Café breakfast menu in French and Arabic, served 7–11am, warm colors, 3 rotating slides",
  "Exam period notice: quiet please, library hours, and a countdown to the first exam",
  "Retail promo: -30% weekend sale on shoes, bold red, with a clock in the corner",
];

async function aiCreate() {
  let st = { used_this_month: 0, monthly_limit: 0 };
  try { st = await api("/ai/status"); } catch {}
  const left = Math.max(0, st.monthly_limit - st.used_this_month);
  const r = await modal({
    title: "Create a layout with AI", submit: left ? "Design it" : "", wide: true,
    body: `<div class="field"><label>Describe the screen</label><textarea name="prompt" required minlength="5" style="min-height:120px" placeholder="Who is it for, what should it say, which languages, any colors or times…"></textarea></div>
      <div class="addbar" style="margin:-4px 0 14px">${EXAMPLES.map((e, i) => `<button type="button" class="btn sm ghost" data-ex="${i}" title="${esc(e)}">${esc(e.split(":")[0].split(",")[0].slice(0, 42))}…</button>`).join("")}</div>
      <div class="fields3">
        <div class="field"><label>Screen</label><select name="res">
          <option value="1920x1080">Landscape 1920×1080</option><option value="1080x1920">Portrait 1080×1920</option>
          <option value="3840x2160">4K landscape</option><option value="3840x1080">Video wall 2×1</option></select></div>
        <div class="field"><label>On-screen language</label><select name="language">
          <option value="the same language as the brief">Same as my brief</option><option>French</option><option>Arabic</option><option>English</option>
          <option value="French and Arabic (bilingual on each slide)">French + Arabic</option><option value="French, Arabic and English">French + Arabic + English</option></select></div>
        <div class="field"><label>&nbsp;</label><label class="chk"><input type="checkbox" name="use_media" checked> Use photos & videos from my library</label></div>
      </div>
      <div class="small muted">${left ? `${left} of ${st.monthly_limit} AI designs left this month.` : `You've used all ${st.monthly_limit} AI designs for this month.`} The result is a draft — nothing goes on a screen until you assign it.</div>
      <div class="ai-progress small" hidden><span class="spin"></span> Designing your layout… this usually takes 20–60 seconds.</div>`,
    onOpen: (f) => f.querySelectorAll("[data-ex]").forEach((b) => b.onclick = () => { f.prompt.value = EXAMPLES[+b.dataset.ex]; f.prompt.focus(); }),
    onSubmit: async (f) => {
      const v = formData(f);
      const [width, height] = v.res.split("x").map(Number);
      f.querySelector(".ai-progress").hidden = false;
      f.querySelectorAll("textarea,select,input").forEach((x) => (x.readOnly = true));
      try {
        return await api("/ai/layouts", { method: "POST", body: { prompt: v.prompt, width, height, language: v.language, use_media: !!v.use_media } });
      } finally {
        f.querySelector(".ai-progress").hidden = true;
        f.querySelectorAll("textarea,select,input").forEach((x) => (x.readOnly = false));
      }
    },
  });
  if (r?.layout) {
    store.cache.aiNote = { layoutId: r.layout.id, summary: r.summary };
    invalidate("layouts", "playlists");
    location.hash = `#/layouts/${r.layout.id}`;
  }
}
