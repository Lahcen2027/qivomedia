import { api, h, esc, modal, confirmDialog, guard, formData, load, invalidate, can } from "../core.js";
import { activeLayouts } from "/shared/schedule.js";

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const COLORS = ["#7c5cff", "#06b6d4", "#f59e0b", "#10b981", "#ef4444", "#ec4899", "#3b82f6", "#84cc16"];

export async function render(root) {
  invalidate("schedules", "displays", "groups", "layouts");
  const [scheds, displays, groups, layouts] = await Promise.all([load("schedules"), load("displays"), load("groups"), load("layouts")]);
  const lname = (id) => layouts.find((l) => l.id === id)?.name || "?";
  const color = (id) => COLORS[Math.max(0, layouts.findIndex((l) => l.id === id)) % COLORS.length];
  const target = (s) => s.target_type === "all" ? "All screens" : s.target_type === "group" ? `Group: ${groups.find((g) => g.id === s.target_id)?.name || "?"}` : `Screen: ${displays.find((d) => d.id === s.target_id)?.name || "?"}`;
  const when = (s) => {
    const days = s.days_mask === 127 ? "Every day" : DAYS.filter((_, i) => (s.days_mask >> i) & 1).join(", ");
    const time = s.time_from ? `${s.time_from}–${s.time_to}` : "all day";
    const range = s.starts_at || s.ends_at ? ` · ${s.starts_at ? s.starts_at.replace("T", " ") : "…"} → ${s.ends_at ? s.ends_at.replace("T", " ") : "…"}` : "";
    return `${days} · ${time}${range}`;
  };
  const rerender = () => { root.innerHTML = ""; render(root); };

  const page = h(`<div>
    <div class="head"><div><h1>Schedule</h1><p>Day-part layouts by date, weekday and time window. Higher priority wins; equal priorities rotate.</p></div>
      ${can.edit() ? `<button class="btn primary" data-new ${layouts.length ? "" : "disabled title='Create a layout first'"}>+ New schedule</button>` : ""}</div>
    <div class="card" style="margin-bottom:18px">
      <div class="row" style="margin-bottom:12px"><h2 class="grow">This week</h2>
        <select data-view style="max-width:280px"><option value="">All schedules (no default)</option>${displays.map((d) => `<option value="${d.id}">${esc(d.name)}</option>`).join("")}</select></div>
      <div class="weekwrap"></div>
      <div class="legend row small" style="margin-top:10px"></div>
    </div>
    <div class="card flush">${scheds.length ? `<table><thead><tr><th>Name</th><th>Layout</th><th>Target</th><th>When</th><th>Priority</th><th></th></tr></thead><tbody>
      ${scheds.map((s) => `<tr data-id="${s.id}" style="${s.enabled ? "" : "opacity:.5"}"><td><b>${esc(s.name)}</b>${s.enabled ? "" : ` <span class="badge">paused</span>`}</td>
        <td><span class="dot" style="background:${color(s.layout_id)}"></span> ${esc(lname(s.layout_id))}</td><td>${esc(target(s))}</td>
        <td class="small">${esc(when(s))}</td><td><span class="badge acc">${s.priority}</span></td>
        <td class="t-actions">${can.edit() ? `<button class="btn sm" data-e>Edit</button><button class="btn sm ghost danger" data-d>Delete</button>` : ""}</td></tr>`).join("")}
      </tbody></table>` : `<div class="empty"><h3>Nothing scheduled</h3><p>Screens play their default layout until you add a schedule.</p></div>`}</div>
  </div>`);
  root.appendChild(page);

  function drawWeek() {
    const did = page.querySelector("[data-view]").value;
    const d = displays.find((x) => x.id === did);
    const list = d ? scheds.filter((s) => s.target_type === "all" || (s.target_type === "group" && s.target_id === d.group_id) || (s.target_type === "display" && s.target_id === d.id)) : scheds;
    const m = { schedules: list, layouts: Object.fromEntries(layouts.map((l) => [l.id, l])), default_layout_id: d?.default_layout_id };
    const now = new Date();
    const monday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - ((now.getDay() + 6) % 7));
    const H = 480, SLOT = 15, N = (24 * 60) / SLOT;
    let html = `<div class="week"><div class="hd"></div>${DAYS.map((x, i) => { const dt = new Date(monday.getTime() + i * 864e5); return `<div class="hd">${x} ${dt.getDate()}</div>`; }).join("")}`;
    html += `<div class="hrs">${[0, 3, 6, 9, 12, 15, 18, 21].map((hr) => `<span style="top:${(hr / 24) * H}px">${String(hr).padStart(2, "0")}:00</span>`).join("")}</div>`;
    const used = new Set();
    for (let day = 0; day < 7; day++) {
      html += `<div class="col">${[3, 6, 9, 12, 15, 18, 21].map((hr) => `<div class="gl" style="top:${(hr / 24) * H}px"></div>`).join("")}`;
      let runStart = 0, runKey = null;
      const flush = (end) => {
        if (runKey) {
          const ids = runKey.split(",");
          ids.forEach((x) => used.add(x));
          const bg = ids.length > 1 ? `repeating-linear-gradient(45deg,${ids.map((x, i) => `${color(x)} ${i * 8}px ${(i + 1) * 8}px`).join(",")})` : color(ids[0]);
          html += `<div class="blk" style="top:${(runStart / N) * H}px;height:${((end - runStart) / N) * H - 1}px;background:${bg}" title="${esc(ids.map(lname).join(" ⇄ "))}">${(end - runStart) * SLOT >= 45 ? esc(ids.map(lname).join(" ⇄ ")) : ""}</div>`;
        }
      };
      for (let s = 0; s <= N; s++) {
        let key = null;
        if (s < N) {
          const t = new Date(monday.getTime() + day * 864e5 + s * SLOT * 6e4 + 6e4);
          const r = activeLayouts(m, t);
          key = r.layouts.length ? r.layouts.join(",") : null;
        }
        if (key !== runKey || s === N) { flush(s); runStart = s; runKey = key; }
      }
      if (day === (now.getDay() + 6) % 7) html += `<div class="now" style="top:${((now.getHours() * 60 + now.getMinutes()) / 1440) * H}px"></div>`;
      html += `</div>`;
    }
    html += `</div>`;
    page.querySelector(".weekwrap").innerHTML = html;
    page.querySelector(".legend").innerHTML = [...used].map((x) => `<span><span class="dot" style="background:${color(x)}"></span> ${esc(lname(x))}</span>`).join("") || `<span class="muted">Nothing plays${d ? "" : " — pick a screen to include its default layout"}.</span>`;
  }
  page.querySelector("[data-view]").onchange = drawWeek;
  if (displays.length) page.querySelector("[data-view]").value = displays[0].id;
  drawWeek();

  page.querySelector("[data-new]")?.addEventListener("click", () => form(null, { layouts, displays, groups }).then((ok) => ok && rerender()));
  page.querySelectorAll("tr[data-id]").forEach((tr) => {
    const s = scheds.find((x) => x.id === tr.dataset.id);
    tr.querySelector("[data-e]")?.addEventListener("click", () => form(s, { layouts, displays, groups }).then((ok) => ok && rerender()));
    tr.querySelector("[data-d]")?.addEventListener("click", async () => {
      if (!(await confirmDialog("Delete schedule?", s.name))) return;
      await guard(() => api(`/schedules/${s.id}`, { method: "DELETE" }), "Deleted");
      rerender();
    });
  });
}

export function targetFields(s, displays, groups) {
  return `<div class="fields2"><div class="field"><label>Target</label><select name="target_type">
      <option value="all" ${s?.target_type === "all" ? "selected" : ""}>All screens</option>
      <option value="group" ${s?.target_type === "group" ? "selected" : ""} ${groups.length ? "" : "disabled"}>Display group</option>
      <option value="display" ${s?.target_type === "display" ? "selected" : ""} ${displays.length ? "" : "disabled"}>Single screen</option></select></div>
    <div class="field"><label>&nbsp;</label><select name="target_id"></select></div></div>`;
}

export function wireTarget(f, s, displays, groups) {
  const tt = f.querySelector("[name=target_type]"), ti = f.querySelector("[name=target_id]");
  const sync = () => {
    const list = tt.value === "group" ? groups : tt.value === "display" ? displays : [];
    ti.innerHTML = list.map((x) => `<option value="${x.id}" ${x.id === s?.target_id ? "selected" : ""}>${esc(x.name)}</option>`).join("");
    ti.disabled = !list.length;
  };
  tt.onchange = sync; sync();
}

function form(s, { layouts, displays, groups }) {
  const mask = s?.days_mask ?? 127;
  return modal({
    title: s ? "Edit schedule" : "New schedule",
    wide: true,
    body: `<div class="fields2"><div class="field"><label>Name</label><input name="name" required value="${esc(s?.name || "")}" placeholder="Breakfast menu"></div>
      <div class="field"><label>Layout</label><select name="layout_id">${layouts.map((l) => `<option value="${l.id}" ${l.id === s?.layout_id ? "selected" : ""}>${esc(l.name)}</option>`).join("")}</select></div></div>
      ${targetFields(s, displays, groups)}
      <label>Days</label><div class="days field">${DAYS.map((d, i) => `<label><input type="checkbox" name="d${i}" ${(mask >> i) & 1 ? "checked" : ""}><span>${d}</span></label>`).join("")}</div>
      <div class="fields2"><div class="field"><label>Daily from (blank = all day)</label><input type="time" name="time_from" value="${esc(s?.time_from || "")}"></div>
      <div class="field"><label>Daily until (overnight OK, e.g. 22:00→06:00)</label><input type="time" name="time_to" value="${esc(s?.time_to || "")}"></div></div>
      <div class="fields2"><div class="field"><label>Starts (optional)</label><input type="datetime-local" name="starts_at" value="${esc(s?.starts_at || "")}"></div>
      <div class="field"><label>Ends (optional)</label><input type="datetime-local" name="ends_at" value="${esc(s?.ends_at || "")}"></div></div>
      <div class="fields2"><div class="field"><label>Priority (0–100, higher wins)</label><input type="number" name="priority" min="0" max="100" value="${s?.priority ?? 10}"></div>
      <div class="field"><label>&nbsp;</label><label class="chk"><input type="checkbox" name="enabled" ${s?.enabled === false ? "" : "checked"}> Enabled</label></div></div>`,
    onOpen: (f) => wireTarget(f, s, displays, groups),
    onSubmit: (f) => {
      const v = formData(f);
      let days = 0;
      for (let i = 0; i < 7; i++) if (v[`d${i}`]) days |= 1 << i;
      const body = { name: v.name, layout_id: v.layout_id, target_type: v.target_type, target_id: v.target_id || null, days_mask: days,
        time_from: v.time_from || null, time_to: v.time_to || null, starts_at: v.starts_at || null, ends_at: v.ends_at || null,
        priority: parseInt(v.priority || "0", 10), enabled: !!v.enabled };
      return s ? api(`/schedules/${s.id}`, { method: "PUT", body }) : api("/schedules", { method: "POST", body });
    },
  });
}
