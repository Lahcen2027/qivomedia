import { api, h, esc, ago, modal, confirmDialog, guard, formData, load, invalidate, can, toast, store } from "../core.js";
import { targetFields, wireTarget } from "./schedules.js";

const KINDS = [
  { kind: "fire", label: "Fire", sub: "Evacuate now", color: "red" },
  { kind: "evacuation", label: "Evacuation", sub: "Go to assembly point", color: "red" },
  { kind: "lockdown", label: "Lockdown", sub: "Stay inside, lock doors", color: "red" },
  { kind: "weather", label: "Severe weather", sub: "Stay indoors", color: "amber" },
  { kind: "custom", label: "Custom alert", sub: "Write your own", color: "blue" },
];
const BG = { red: "#b91c1c", amber: "#f59e0b", blue: "#1d4ed8" };
const FG = { red: "#fff", amber: "#1c1206", blue: "#fff" };
const WARN = '<svg viewBox="0 0 24 24" width="28" height="28" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M10.3 3.9L1.8 18a2 2 0 001.7 3h17a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z"/><path d="M12 9v4M12 17h.01"/></svg>';

export async function render(root) {
  invalidate("displays", "groups");
  const [active, history, templates, displays, groups] = await Promise.all([
    api("/alerts?active=true"), api("/alerts"), api("/alerts/templates"), load("displays"), load("groups"),
  ]);
  const hook = can.admin() ? await api("/alerts/hook").catch(() => null) : null;
  const target = (a) => a.target_type === "all" ? "All screens" : a.target_type === "group" ? `Group: ${groups.find((g) => g.id === a.target_id)?.name || "?"}` : `Screen: ${displays.find((d) => d.id === a.target_id)?.name || "?"}`;
  const rerender = () => { root.innerHTML = ""; render(root); };
  const hookUrl = hook ? `${location.origin}/api/alert-hooks/${hook.key}` : "";

  const page = h(`<div>
    <div class="head"><div><h1>Emergency alerts</h1><p>Take over screens instantly, above every schedule, until you clear the alert. Messages appear in French, Arabic and English.</p></div>
      ${active.length && can.edit() ? `<button class="btn" data-clearall style="border-color:#34d399;color:#34d399">Clear all alerts</button>` : ""}</div>

    ${active.length ? `<div class="stack" style="margin-bottom:22px">${active.map((a) => `
      <div class="card" data-active="${a.id}" style="background:${BG[a.color]};color:${FG[a.color]};border-color:transparent">
        <div class="row" style="align-items:flex-start;gap:16px">
          <span style="flex:none">${WARN}</span>
          <div class="grow"><div style="font-size:20px;font-weight:700">${esc(a.lines[0]?.title)} ${a.test ? `<span class="badge" style="background:#fff;color:#111;border:0">TEST</span>` : ""}</div>
            <div style="opacity:.9;margin-top:4px">${esc(a.lines[0]?.message)}</div>
            <div class="small" style="opacity:.85;margin-top:8px">${esc(target(a))} · started ${ago(a.started_at)} · via ${esc(a.source)}</div></div>
          ${can.edit() ? `<button class="btn" data-clear style="background:#fff;color:#111;border:0;font-weight:700;padding:10px 16px">Clear alert</button>` : ""}
        </div></div>`).join("")}</div>` : `<div class="card" style="margin-bottom:22px;display:flex;align-items:center;gap:12px"><span class="dot on"></span><b>No active alerts.</b><span class="muted">All screens are playing their normal content.</span></div>`}

    ${can.edit() ? `<h2 style="margin-bottom:12px">Start an alert</h2>
    <div class="grid" style="grid-template-columns:repeat(auto-fill,minmax(200px,1fr));margin-bottom:26px">
      ${KINDS.map((k) => `<button class="alert-kind" data-kind="${k.kind}" style="background:${BG[k.color]};color:${FG[k.color]}">${WARN}<b>${k.label}</b><span>${k.sub}</span></button>`).join("")}
    </div>` : ""}

    ${hook ? `<div class="card" style="margin-bottom:22px">
      <div class="row" style="margin-bottom:10px"><h2 class="grow">Connect your emergency system</h2><button class="btn sm" data-rotate>Change secret URL</button></div>
      <p class="muted" style="margin-top:0">Fire panels, campus safety apps or alert platforms can raise and clear alerts with one HTTP call. Standard <b>CAP 1.2</b> XML alerts are accepted as-is (<code>msgType</code> Cancel clears).</p>
      <div class="copy" style="margin-bottom:12px"><input readonly value="${esc(hookUrl)}"><button class="btn sm" data-copy>Copy</button></div>
      <details><summary class="small muted" style="cursor:pointer">Examples</summary>
<pre class="mono" style="background:var(--bg2);padding:12px;border-radius:8px;overflow:auto;margin:8px 0 0"># Start a fire alert on every screen
curl -X POST ${esc(hookUrl)} -H "Content-Type: application/json" \\
  -d '{"kind":"fire"}'

# Start a custom alert on one group, as a drill
curl -X POST ${esc(hookUrl)} -H "Content-Type: application/json" \\
  -d '{"kind":"custom","title":"Gas leak — Building B","message":"Leave Building B now.","target_type":"group","target_id":"&lt;group id&gt;","test":true}'

# Clear all alerts
curl -X POST ${esc(hookUrl)} -H "Content-Type: application/json" -d '{"action":"clear"}'

# CAP 1.2 feed
curl -X POST ${esc(hookUrl)} -H "Content-Type: application/xml" --data-binary @alert.xml</pre></details>
    </div>` : ""}

    <div class="card flush"><div style="padding:14px 16px"><h2>History</h2></div><table><thead><tr><th>Alert</th><th>Target</th><th>Source</th><th>Started</th><th>Cleared</th></tr></thead><tbody>
      ${history.map((a) => `<tr><td><span class="dot" style="background:${BG[a.color]}"></span> ${esc(a.lines[0]?.title)} ${a.test ? `<span class="badge">test</span>` : ""}</td><td>${esc(target(a))}</td><td>${esc(a.source)}</td>
        <td class="muted">${new Date(a.started_at).toLocaleString()}</td><td>${a.cleared_at ? `<span class="muted">${new Date(a.cleared_at).toLocaleString()}</span>` : `<span class="badge bad">Active</span>`}</td></tr>`).join("") || `<tr><td colspan="5" class="muted">No alerts yet. Consider a test alert during your next drill.</td></tr>`}
    </tbody></table></div>
  </div>`);
  root.appendChild(page);

  page.querySelector("[data-clearall]")?.addEventListener("click", async () => {
    await guard(() => api("/alerts/clear-all", { method: "POST" }), "All alerts cleared — screens show “All clear”");
    rerender();
  });
  page.querySelectorAll("[data-active]").forEach((c) => c.querySelector("[data-clear]")?.addEventListener("click", async () => {
    await guard(() => api(`/alerts/${c.dataset.active}/clear`, { method: "POST" }), "Alert cleared — screens show “All clear”");
    rerender();
  }));
  page.querySelector("[data-copy]")?.addEventListener("click", () => navigator.clipboard.writeText(hookUrl).then(() => toast("Alert webhook URL copied")));
  page.querySelector("[data-rotate]")?.addEventListener("click", async () => {
    if (!(await confirmDialog("Change the secret URL?", "Systems using the current URL will stop working until you update them.", "Change URL"))) return;
    await guard(() => api("/alerts/hook/rotate", { method: "POST" }), "New URL created");
    rerender();
  });
  page.querySelectorAll("[data-kind]").forEach((b) => b.onclick = () => startDialog(b.dataset.kind, templates, displays, groups).then((ok) => ok && rerender()));
}

function startDialog(kind, templates, displays, groups) {
  const t = templates.find((x) => x.kind === kind);
  const k = KINDS.find((x) => x.kind === kind);
  const preview = t ? `<div style="background:${BG[t.color]};color:${FG[t.color]};border-radius:10px;padding:16px;margin-bottom:16px">
      ${t.lines.map((l) => `<div ${l.lang === "ar" ? 'dir="rtl"' : ""} style="margin-bottom:8px"><b>${esc(l.title)}</b> — ${esc(l.message)}</div>`).join("")}</div>` : `
      <div class="field"><label>Title</label><input name="title" required maxlength="120" placeholder="Gas leak — Building B"></div>
      <div class="field"><label>Message</label><textarea name="message" maxlength="600" placeholder="Leave Building B now by the nearest exit."></textarea></div>
      <div class="field"><label>Color</label><select name="color"><option value="red">Red — danger</option><option value="amber">Amber — warning</option><option value="blue">Blue — information</option></select></div>`;
  return modal({
    title: `${k.label} alert`, submit: "Send alert now", danger: true, wide: true,
    body: `${preview}${targetFields({ target_type: "all" }, displays, groups)}
      <div class="row" style="gap:22px"><label class="chk"><input type="checkbox" name="test"> Test / drill — screens show “TEST · EXERCICE”, no siren</label>
      <label class="chk"><input type="checkbox" name="sound" checked> Siren sound</label></div>
      <p class="small muted" style="margin-bottom:0">Screens switch within about a second and stay on the alert until it's cleared — even if they lose the network.</p>`,
    onOpen: (f) => wireTarget(f, { target_type: "all" }, displays, groups),
    onSubmit: async (f) => {
      const v = formData(f);
      const r = await api("/alerts", { method: "POST", body: { kind, title: v.title, message: v.message, color: v.color, target_type: v.target_type, target_id: v.target_id || null, test: !!v.test, sound: !!v.sound } });
      toast(`${v.test ? "Test alert" : "Alert"} sent to ${r.screens} screen(s)`, !r.screens);
    },
  });
}

/** Red bar across the top of every dashboard page while any alert is active. */
export async function refreshAlertBar() {
  const bar = document.getElementById("alertbar");
  if (!bar || !store.me) return;
  let active = [];
  try { active = await api("/alerts?active=true"); } catch { return; }
  if (!active.length) { bar.innerHTML = ""; bar.hidden = true; return; }
  const a = active[0];
  bar.hidden = false;
  bar.innerHTML = `<div class="alertbar" style="background:${BG[a.color]};color:${FG[a.color]}">${WARN}
    <b>${active.length > 1 ? `${active.length} alerts active` : `${esc(a.lines[0]?.title)} alert active`}${a.test ? " (test)" : ""}</b>
    <span class="grow" style="opacity:.9">Screens are showing the alert.</span>
    <a href="#/alerts" style="color:inherit;text-decoration:underline">Manage</a>
    ${can.edit() ? `<button class="btn sm" data-barclear style="background:#fff;color:#111;border:0;font-weight:700">Clear ${active.length > 1 ? "all" : "alert"}</button>` : ""}</div>`;
  bar.querySelector("[data-barclear]")?.addEventListener("click", async () => {
    await guard(() => api("/alerts/clear-all", { method: "POST" }), "Cleared — screens show “All clear”");
    refreshAlertBar();
    if (location.hash === "#/alerts") window.dispatchEvent(new HashChangeEvent("hashchange"));
  });
}
