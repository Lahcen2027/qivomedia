import { api, h, esc, ago, modal, confirmDialog, guard, formData, load, invalidate, can, toast } from "../core.js";

const nowPlaying = {}; // display_id -> layout_id (from live status)

export async function render(root) {
  invalidate("displays", "groups", "layouts");
  const [displays, groups, layouts] = await Promise.all([load("displays"), load("groups"), load("layouts")]);
  const gname = (id) => groups.find((g) => g.id === id)?.name;
  const lname = (id) => layouts.find((l) => l.id === id)?.name;
  const rerender = () => { root.innerHTML = ""; render(root); };

  const page = h(`<div>
    <div class="head"><div><h1>Displays</h1><p>Pair screens, organize them into groups and control them live.</p></div>
      <div class="row">${can.admin() ? `<button class="btn" data-a="group">New group</button><button class="btn primary" data-a="add">+ Add screen</button>` : ""}</div></div>
    ${groups.length ? `<div class="row" style="margin-bottom:14px">${groups.map((g) => `<span class="badge acc" data-g="${g.id}" style="cursor:pointer" title="Click to rename / delete">${esc(g.name)} · ${g.display_count}</span>`).join("")}</div>` : ""}
    <div class="card flush">${displays.length ? `<table><thead><tr><th>Screen</th><th>Status</th><th>Group</th><th>Default layout</th><th>Now playing</th><th>Device</th><th></th></tr></thead><tbody>
      ${displays.map((d) => `<tr data-id="${d.id}">
        <td><b>${esc(d.name)}</b><div class="small faint">seen ${ago(d.last_seen)}</div></td>
        <td class="st">${status(d)}</td>
        <td>${gname(d.group_id) ? `<span class="badge">${esc(gname(d.group_id))}</span>` : `<span class="faint">—</span>`}</td>
        <td>${esc(lname(d.default_layout_id) || "—")}</td>
        <td class="np muted">${esc(lname(nowPlaying[d.id]) || "—")}</td>
        <td class="small muted">${deviceCell(d)}</td>
        <td class="t-actions">
          ${caps(d).includes("screenshot") ? `<button class="btn sm" data-shot title="See what this screen shows right now">Screenshot</button>` : ""}
          <button class="btn sm" data-c="identify" title="Show name on screen">Identify</button>
          <button class="btn sm" data-c="reload">Reload</button>
          ${can.admin() ? `<button class="btn sm" data-e>Edit</button>` : ""}
        </td></tr>`).join("")}
    </tbody></table>` : `<div class="empty"><h3>No screens paired yet</h3>
      <p>On any TV, kiosk, Android box, Raspberry Pi or PC browser, open<br><code style="font-size:15px">${esc(location.origin)}/player</code><br>then click <b>Add screen</b> and type the 6-character code it shows.</p></div>`}</div>
  </div>`);
  root.appendChild(page);

  page.querySelector("[data-a=add]")?.addEventListener("click", () => addScreen(groups, layouts).then((ok) => ok && rerender()));
  page.querySelector("[data-a=group]")?.addEventListener("click", () =>
    modal({ title: "New display group", body: `<div class="field"><label>Name</label><input name="name" required placeholder="Lobby screens"></div>`,
      submit: "Create", onSubmit: (f) => api("/groups", { method: "POST", body: formData(f) }) }).then((ok) => ok && rerender()));
  page.querySelectorAll("[data-g]").forEach((b) => b.onclick = () => editGroup(groups.find((g) => g.id === b.dataset.g)).then((ok) => ok && rerender()));

  page.querySelectorAll("tr[data-id]").forEach((tr) => {
    const d = displays.find((x) => x.id === tr.dataset.id);
    tr.querySelectorAll("[data-c]").forEach((b) => b.onclick = async () => {
      const r = await guard(() => api(`/displays/${d.id}/command`, { method: "POST", body: { command: b.dataset.c } }));
      toast(r.delivered ? `Sent “${b.dataset.c}” to ${d.name}` : `${d.name} is offline — command not delivered`, !r.delivered);
    });
    tr.querySelector("[data-e]")?.addEventListener("click", () => editScreen(d, groups, layouts).then((ok) => ok && rerender()));
    tr.querySelector("[data-shot]")?.addEventListener("click", () => screenshotDialog(d));
  });

  const onLive = (e) => {
    if (!root.isConnected) return window.removeEventListener("qivo:live", onLive);
    const m = e.detail;
    const tr = root.querySelector(`tr[data-id="${m.display_id}"]`);
    if (m.type === "display_status") {
      nowPlaying[m.display_id] = m.layout_id;
      if (tr) tr.querySelector(".np").textContent = lname(m.layout_id) || "—";
    }
    if (m.type === "display_presence" && tr) {
      const d = displays.find((x) => x.id === m.display_id);
      if (d) { d.online = m.online; tr.querySelector(".st").innerHTML = status(d); }
    }
  };
  window.addEventListener("qivo:live", onLive);
}

// ---- native players (Windows app) ----
const nat = (d) => d.player_info?.native || null;
const caps = (d) => nat(d)?.capabilities || [];
const pct = (v) => (v == null ? "–" : `${Math.round(v)}%`);
const primaryMonitor = (n) => (n?.monitors || []).find((m) => m.primary) || n?.monitors?.[0];

function deviceCell(d) {
  const n = nat(d);
  if (!n) return `${esc(d.player_info?.screen || "")}<div class="faint">${esc(d.player_info?.timezone || "")}</div>`;
  const m = primaryMonitor(n);
  const res = m ? `${m.width}×${m.height}${m.hz ? ` @ ${Math.round(m.hz)}Hz` : ""}` : "";
  const warn = (n.disk_free_gb != null && n.disk_free_gb < 5) || (n.temperature_c != null && n.temperature_c >= 85);
  return `<span class="badge acc">Windows app</span> ${esc(res)}
    <div class="faint ${warn ? "bad" : ""}">CPU ${pct(n.cpu_percent)}${n.temperature_c != null ? ` · ${n.temperature_c}°C` : ""}${n.disk_free_gb != null ? ` · ${n.disk_free_gb} GB free` : ""}</div>`;
}

function fmtUptime(s) {
  if (s == null) return "–";
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  return d ? `${d}d ${h}h` : h ? `${h}h ${m}m` : `${m}m`;
}

function healthPanel(d) {
  const n = nat(d);
  if (!n) return `<div class="small muted">Device</div><div class="small mono" style="word-break:break-all">${esc(d.player_info?.user_agent || "unknown")}</div>`;
  const m = primaryMonitor(n);
  const row = (k, v) => `<div class="small muted">${k}</div><div class="small">${v}</div>`;
  const low4k = m && m.width >= 3840 && m.scale && m.scale !== 1 && m.scale !== 1.5 && m.scale !== 2;
  return `<div class="small muted" style="margin-bottom:6px">Device health · reported ${ago(d.last_seen)}</div>
    <div style="display:grid;grid-template-columns:auto 1fr auto 1fr;gap:4px 14px">
      ${row("Computer", esc(n.hostname || "–"))}${row("System", esc(n.os || n.platform))}
      ${row("Player", `Windows app v${esc(n.app_version)}`)}${row("Uptime", fmtUptime(n.uptime_s))}
      ${row("CPU", `${pct(n.cpu_percent)} of ${n.cpu_cores} cores`)}${row("Memory", `${n.memory_used_mb} / ${n.memory_total_mb} MB`)}
      ${row("Disk free", n.disk_free_gb != null ? `${n.disk_free_gb} of ${n.disk_total_gb} GB` : "–")}${row("Temperature", n.temperature_c != null ? `${n.temperature_c} °C` : "not reported")}
      ${row("Screen", m ? `${m.width}×${m.height}${m.hz ? ` @ ${Math.round(m.hz)} Hz` : ""}, scale ${Math.round((m.scale || 1) * 100)}%` : "–")}${row("Screens", String(n.monitors?.length || 0))}
    </div>
    ${low4k ? `<p class="small" style="color:var(--warn)">Unusual Windows scaling on a 4K screen: set it to 100%, 150% or 200% for pixel-sharp text.</p>` : ""}`;
}

function nativeButtons(d) {
  if (!can.admin()) return "";
  const c = caps(d);
  return [
    c.includes("restart_app") && `<button type="button" class="btn sm" data-n="restart_app">Restart player</button>`,
    c.includes("display_off") && `<button type="button" class="btn sm" data-n="display_off">Screen off</button>`,
    c.includes("display_on") && `<button type="button" class="btn sm" data-n="display_on">Screen on</button>`,
    c.includes("reboot") && `<button type="button" class="btn sm danger" data-n="reboot">Restart computer</button>`,
  ].filter(Boolean).join("");
}

async function sendNative(d, command) {
  if (command === "reboot" && !(await confirmDialog("Restart this computer?", `${d.name} will go dark for about a minute while Windows restarts.`, "Restart"))) return;
  const r = await guard(() => api(`/displays/${d.id}/command`, { method: "POST", body: { command } }));
  if (r) toast(r.delivered ? `Sent to ${d.name}` : `${d.name} is offline — command not delivered`, !r.delivered);
}

/** Ask the screen for a fresh screenshot and show it as soon as it arrives. */
function screenshotDialog(d) {
  const src = (at) => `/api/displays/${d.id}/screenshot?t=${encodeURIComponent(at)}`;
  let onLive;
  modal({
    title: `What ${d.name} shows`, wide: true, submit: "Take another", cancel: "Close",
    body: `<div class="shot" style="aspect-ratio:16/9;background:#000;border-radius:10px;display:grid;place-items:center;overflow:hidden">
        ${d.screenshot_at ? `<img alt="Screenshot" src="${src(d.screenshot_at)}" style="width:100%;height:100%;object-fit:contain">` : `<span class="muted">No screenshot yet</span>`}
      </div><p class="small muted" data-when style="margin:8px 0 0">${d.screenshot_at ? `Taken ${ago(d.screenshot_at)} · ` : ""}asking the screen for a new one…</p>`,
    onOpen: (f, close) => {
      const ask = async () => {
        f.querySelector("[data-when]").textContent = "Asking the screen for a new screenshot…";
        const r = await guard(() => api(`/displays/${d.id}/command`, { method: "POST", body: { command: "screenshot" } }));
        if (r && !r.delivered) f.querySelector("[data-when]").textContent = `${d.name} is offline — showing the last screenshot.`;
      };
      onLive = (e) => {
        const m = e.detail;
        if (m.type !== "display_screenshot" || m.display_id !== d.id) return;
        if (!f.isConnected) return window.removeEventListener("qivo:live", onLive);
        d.screenshot_at = m.at;
        f.querySelector(".shot").innerHTML = `<img alt="Screenshot" src="${src(m.at)}" style="width:100%;height:100%;object-fit:contain">`;
        f.querySelector("[data-when]").textContent = `Taken just now (${new Date(m.at).toLocaleTimeString()})`;
      };
      window.addEventListener("qivo:live", onLive);
      f._ask = ask;
      ask();
    },
    onSubmit: (f) => { f._ask(); return false; },
  }).then(() => window.removeEventListener("qivo:live", onLive));
}

function status(d) {
  if (d.status === "disabled") return `<span class="badge warn">Disabled</span>`;
  return d.online ? `<span class="badge ok"><span class="dot on"></span>Online</span>` : `<span class="badge bad"><span class="dot"></span>Offline</span>`;
}

const opts = (list, sel, none = "— None —") =>
  `<option value="">${none}</option>` + list.map((x) => `<option value="${x.id}" ${x.id === sel ? "selected" : ""}>${esc(x.name)}</option>`).join("");

function addScreen(groups, layouts) {
  return modal({
    title: "Add a screen",
    submit: "Pair screen",
    body: `<p class="muted" style="margin-top:0">Open <code>${esc(location.origin)}/player</code> on the screen. It shows a pairing code.</p>
      <div class="field"><label>Pairing code</label><input name="code" required maxlength="6" style="font:700 22px ui-monospace,monospace;letter-spacing:.3em;text-transform:uppercase" placeholder="ABC123"></div>
      <div class="field"><label>Screen name</label><input name="name" placeholder="Front window"></div>
      <div class="fields2">
        <div class="field"><label>Group</label><select name="group_id">${opts(groups)}</select></div>
        <div class="field"><label>Default layout</label><select name="default_layout_id">${opts(layouts)}</select></div>
      </div>`,
    onSubmit: async (f) => {
      const r = await api("/displays/claim", { method: "POST", body: formData(f) });
      toast(`${r.name} paired — it will start playing in a moment`);
    },
  });
}

function editScreen(d, groups, layouts) {
  return modal({
    title: `Edit ${d.name}`,
    body: `<div class="field"><label>Name</label><input name="name" value="${esc(d.name)}" required></div>
      <div class="fields2">
        <div class="field"><label>Group</label><select name="group_id">${opts(groups, d.group_id)}</select></div>
        <div class="field"><label>Default layout</label><select name="default_layout_id">${opts(layouts, d.default_layout_id)}</select></div>
      </div>
      <div class="field"><label class="chk"><input type="checkbox" name="disabled" ${d.status === "disabled" ? "checked" : ""}> Disable this screen</label></div>
      <div class="card" style="background:var(--bg2);margin-top:10px">
        ${healthPanel(d)}
        <div class="row" style="margin-top:12px;flex-wrap:wrap">${nativeButtons(d)}<button type="button" class="btn sm" data-cc>Clear player cache</button><button type="button" class="btn sm danger" data-del>Unpair & delete</button></div>
      </div>`,
    onOpen: (f, close) => {
      f.querySelector("[data-del]").onclick = async () => {
        if (await confirmDialog("Delete screen?", `${d.name} will return to its pairing screen.`)) {
          await guard(() => api(`/displays/${d.id}`, { method: "DELETE" }), "Screen removed");
          close(true);
        }
      };
      f.querySelectorAll("[data-n]").forEach((b) => b.onclick = () => sendNative(d, b.dataset.n));
      f.querySelector("[data-cc]").onclick = () => guard(() => api(`/displays/${d.id}/command`, { method: "POST", body: { command: "clear_cache" } }), "Cache clear sent");
    },
    onSubmit: (f) => {
      const v = formData(f);
      return api(`/displays/${d.id}`, { method: "PATCH", body: {
        name: v.name, group_id: v.group_id || null, default_layout_id: v.default_layout_id || null, status: v.disabled ? "disabled" : "active",
      } });
    },
  });
}

function editGroup(g) {
  return modal({
    title: "Display group",
    body: `<div class="field"><label>Name</label><input name="name" value="${esc(g.name)}" required></div>
      <button type="button" class="btn sm danger" data-del>Delete group</button>`,
    onOpen: (f, close) => f.querySelector("[data-del]").onclick = async () => {
      if (await confirmDialog("Delete group?", "Screens stay paired; schedules that target this group are removed.")) {
        await guard(() => api(`/groups/${g.id}`, { method: "DELETE" }));
        close(true);
      }
    },
    onSubmit: (f) => api(`/groups/${g.id}`, { method: "PATCH", body: formData(f) }),
  });
}
