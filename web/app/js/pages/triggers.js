import { api, h, esc, ago, modal, confirmDialog, guard, formData, load, invalidate, can, toast } from "../core.js";
import { targetFields, wireTarget } from "./schedules.js";

export async function render(root) {
  invalidate("triggers", "displays", "groups", "layouts");
  const [trs, displays, groups, layouts] = await Promise.all([load("triggers"), load("displays"), load("groups"), load("layouts")]);
  const lname = (id) => layouts.find((l) => l.id === id)?.name || "?";
  const hook = (t) => `${location.origin}/api/hooks/${t.hook_key}`;
  const rerender = () => { root.innerHTML = ""; render(root); };

  const page = h(`<div>
    <div class="head"><div><h1>Triggers</h1><p>Instantly take over screens from any system — POS, sensors, fire alarm, queue system, Zapier — with one HTTP call.</p></div>
      ${can.edit() ? `<button class="btn primary" data-new ${layouts.length ? "" : "disabled"}>+ New trigger</button>` : ""}</div>
    <div class="stack">${trs.length ? trs.map((t) => `<div class="card" data-id="${t.id}">
      <div class="row"><h2 class="grow">⚡ ${esc(t.name)}</h2><span class="badge">${t.duration_s}s</span><span class="badge acc">${esc(lname(t.layout_id))}</span>
        <span class="small muted">fired ${ago(t.last_fired)}</span></div>
      <div class="copy" style="margin:12px 0"><input readonly value="${esc(hook(t))}"><button class="btn sm" data-copy>Copy</button></div>
      <details><summary class="small muted" style="cursor:pointer">Example call</summary>
<pre class="mono" style="background:var(--bg2);padding:12px;border-radius:8px;overflow:auto;margin:8px 0 0">curl -X POST ${esc(hook(t))} \\
  -H "Content-Type: application/json" \\
  -d '{"duration_s": 20, "data": {"name": "Order 42", "msg": "Ready for pickup"}}'</pre>
        <p class="small muted">Values in <code>data</code> fill <code>{{name}}</code>-style placeholders in text, ticker, countdown and HTML widgets of the trigger layout.</p></details>
      <div class="row" style="margin-top:12px">${can.edit() ? `<button class="btn sm primary" data-fire>Fire now</button><button class="btn sm" data-e>Edit</button><button class="btn sm ghost danger" data-d>Delete</button>` : ""}</div>
    </div>`).join("") : `<div class="card empty"><h3>No triggers yet</h3><p>Example: when the POS marks an order ready, flash “Order {{name}} ready” on the pickup screen for 20 seconds.</p></div>`}</div></div>`);
  root.appendChild(page);

  page.querySelector("[data-new]")?.addEventListener("click", () => form(null).then((ok) => ok && rerender()));
  page.querySelectorAll("[data-id]").forEach((c) => {
    const t = trs.find((x) => x.id === c.dataset.id);
    c.querySelector("[data-copy]").onclick = () => navigator.clipboard.writeText(hook(t)).then(() => toast("Webhook URL copied"));
    c.querySelector("[data-fire]")?.addEventListener("click", async () => {
      const r = await guard(() => api(`/triggers/${t.id}/fire`, { method: "POST", body: { data: { name: "Test", msg: "Hello from Qivomedia" } } }));
      toast(r.displays ? `Interrupting ${r.displays} screen(s) for ${r.duration_s}s` : "No active screens match this trigger's target", !r.displays);
    });
    c.querySelector("[data-e]")?.addEventListener("click", () => form(t).then((ok) => ok && rerender()));
    c.querySelector("[data-d]")?.addEventListener("click", async () => {
      if (!(await confirmDialog("Delete trigger?", "Its webhook URL stops working immediately."))) return;
      await guard(() => api(`/triggers/${t.id}`, { method: "DELETE" }), "Deleted");
      rerender();
    });
  });

  function form(t) {
    return modal({
      title: t ? "Edit trigger" : "New trigger",
      body: `<div class="field"><label>Name</label><input name="name" required value="${esc(t?.name || "")}" placeholder="Order ready"></div>
        <div class="fields2"><div class="field"><label>Layout to show</label><select name="layout_id">${layouts.map((l) => `<option value="${l.id}" ${l.id === t?.layout_id ? "selected" : ""}>${esc(l.name)}</option>`).join("")}</select></div>
        <div class="field"><label>Duration (seconds)</label><input type="number" name="duration_s" min="1" value="${t?.duration_s ?? 30}"></div></div>
        ${targetFields(t, displays, groups)}`,
      onOpen: (f) => wireTarget(f, t, displays, groups),
      onSubmit: (f) => {
        const v = formData(f);
        const body = { name: v.name, layout_id: v.layout_id, duration_s: parseInt(v.duration_s, 10), target_type: v.target_type, target_id: v.target_id || null };
        return t ? api(`/triggers/${t.id}`, { method: "PUT", body }) : api("/triggers", { method: "POST", body });
      },
    });
  }
}
