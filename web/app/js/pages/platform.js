import { api, h, esc, ago, fmtBytes, modal, confirmDialog, guard, formData, store, toast } from "../core.js";

const PLANS = ["trial", "pilot", "cloud", "education", "onprem", "internal"];
const planBadge = (p) => `<span class="badge ${p === "cloud" || p === "onprem" ? "ok" : p === "trial" ? "warn" : "acc"}">${esc(p)}</span>`;

export async function render(root) {
  if (!store.me.user.platform_admin) {
    root.innerHTML = `<div class="empty"><h3>Platform admins only</h3></div>`;
    return;
  }
  const [o, orgs, usage] = await Promise.all([api("/platform/overview"), api("/platform/orgs"), api("/platform/ai-usage")]);
  const rerender = () => { root.innerHTML = ""; render(root); };

  const page = h(`<div>
    <div class="head"><div><h1>Platform</h1><p>Every customer organization on this Qivomedia server: plans, limits, AI access and health.</p></div>
      <button class="btn primary" data-new>+ New customer</button></div>
    <div class="grid g4" style="margin-bottom:18px">
      ${stat("Organizations", `${o.organizations}${o.suspended ? ` <span class="muted" style="font-size:15px">· ${o.suspended} suspended</span>` : ""}`)}
      ${stat("Screens online", `${o.online}<span class="muted" style="font-size:18px"> / ${o.displays}</span>`)}
      ${stat("Users", o.users)}
      ${stat("Storage", fmtBytes(o.storage_bytes))}
      ${stat("Plays (24h)", o.plays_24h.toLocaleString())}
      ${stat("AI designs this month", `${o.ai_generations_month}`)}
      ${stat("AI tokens this month", `${fmtTokens(o.ai_input_tokens_month)} in · ${fmtTokens(o.ai_output_tokens_month)} out`)}
      ${stat("AI service", o.ai_configured ? `<span class="badge ok">Connected</span>` : `<span class="badge warn">No API key</span>`)}
    </div>
    ${o.ai_configured ? "" : `<div class="card" style="margin-bottom:18px;border-color:rgba(251,191,36,.35)"><b>AI is off for everyone.</b> <span class="muted">Add <code>ANTHROPIC_API_KEY=…</code> to <code>deploy/.env</code> on the server and restart (<code>qivo up -d</code>). Keys are created at console.anthropic.com.</span></div>`}
    <div class="card flush" style="margin-bottom:18px"><table><thead><tr><th>Organization</th><th>Plan</th><th>Screens</th><th>Users</th><th>Storage</th><th>AI this month</th><th>Last screen activity</th><th></th></tr></thead><tbody>
      ${orgs.map((g) => `<tr data-id="${g.id}" style="${g.suspended ? "opacity:.6" : ""}">
        <td><b>${esc(g.name)}</b>${g.is_own ? ` <span class="faint small">(yours)</span>` : ""}${g.suspended ? ` <span class="badge bad">Suspended</span>` : ""}<div class="small muted">${esc(g.owner_email || "")}</div>${g.notes ? `<div class="small faint">${esc(g.notes)}</div>` : ""}</td>
        <td>${planBadge(g.plan)}</td>
        <td><span class="dot ${g.online ? "on" : ""}"></span> ${g.online} / ${g.displays}${g.max_screens != null ? `<span class="muted"> of ${g.max_screens}</span>` : ""}</td>
        <td>${g.users}</td><td>${fmtBytes(g.storage_bytes)}</td>
        <td>${g.ai_enabled ? `${g.ai_used_month} / ${g.ai_monthly_limit}` : `<span class="muted">off</span>`}</td>
        <td class="muted">${ago(g.last_seen)}</td>
        <td class="t-actions"><button class="btn sm" data-e>Edit</button>${g.is_own ? "" : `<button class="btn sm ${g.suspended ? "" : "danger"}" data-s>${g.suspended ? "Reactivate" : "Suspend"}</button>`}</td></tr>`).join("")}
    </tbody></table></div>
    <div class="card flush"><div style="padding:14px 16px"><h2>Recent AI activity</h2></div><table><thead><tr><th>When</th><th>Organization</th><th>Type</th><th>Brief</th><th>Tokens</th><th>Result</th></tr></thead><tbody>
      ${usage.map((u) => `<tr><td class="muted small">${ago(u.created_at)}</td><td>${esc(u.org_name || "deleted")}</td><td>${u.kind === "create" ? "New layout" : "Revision"}</td>
        <td class="small" style="max-width:380px">${esc(u.prompt.length > 120 ? u.prompt.slice(0, 120) + "…" : u.prompt)}</td>
        <td class="small muted">${fmtTokens(u.input_tokens)} / ${fmtTokens(u.output_tokens)}</td>
        <td>${u.ok ? `<span class="badge ok">OK</span>` : `<span class="badge bad" title="${esc(u.error || "")}">Failed</span>`}</td></tr>`).join("") || `<tr><td colspan="6" class="muted">No AI activity yet.</td></tr>`}
    </tbody></table></div>
  </div>`);
  root.appendChild(page);

  page.querySelector("[data-new]").onclick = () => modal({
    title: "New customer organization", submit: "Create customer", wide: true,
    body: `<div class="fields2"><div class="field"><label>Organization</label><input name="org_name" required placeholder="Faculty of Sciences"></div>
      <div class="field"><label>Plan</label><select name="plan">${PLANS.map((p) => `<option ${p === "pilot" ? "selected" : ""}>${p}</option>`).join("")}</select></div></div>
      <div class="fields3"><div class="field"><label>Owner name</label><input name="owner_name" required></div>
      <div class="field"><label>Owner email</label><input name="owner_email" type="email" required></div>
      <div class="field"><label>Temporary password</label><input name="owner_password" minlength="8" required value="${tempPassword()}"></div></div>
      <div class="fields2"><div class="field"><label>Screen limit (blank = unlimited)</label><input name="max_screens" type="number" min="0" placeholder="20"></div>
      <div class="field"><label>Internal notes</label><input name="notes" placeholder="Pilot until March, contact: IT dept"></div></div>
      <p class="small muted" style="margin:0">Send the owner their email and temporary password. They can change it in Team.</p>`,
    onSubmit: async (f) => {
      const v = formData(f);
      await api("/platform/orgs", { method: "POST", body: { ...v, max_screens: v.max_screens === "" ? null : parseInt(v.max_screens, 10) } });
      toast(`${v.org_name} created — owner: ${v.owner_email}`);
    },
  }).then((ok) => ok && rerender());

  page.querySelectorAll("tr[data-id]").forEach((tr) => {
    const g = orgs.find((x) => x.id === tr.dataset.id);
    tr.querySelector("[data-s]")?.addEventListener("click", async () => {
      if (!g.suspended && !(await confirmDialog(`Suspend ${g.name}?`, "Its users can't sign in and its screens show a disabled message until you reactivate it. Nothing is deleted.", "Suspend"))) return;
      await guard(() => api(`/platform/orgs/${g.id}`, { method: "PATCH", body: { suspended: !g.suspended } }), g.suspended ? "Reactivated" : "Suspended");
      rerender();
    });
    tr.querySelector("[data-e]").onclick = () => modal({
      title: `Edit ${g.name}`, wide: true,
      body: `<div class="fields2"><div class="field"><label>Name</label><input name="name" value="${esc(g.name)}"></div>
        <div class="field"><label>Plan</label><select name="plan">${PLANS.map((p) => `<option ${p === g.plan ? "selected" : ""}>${p}</option>`).join("")}</select></div></div>
        <div class="fields3"><div class="field"><label>Screen limit (blank = unlimited)</label><input name="max_screens" type="number" min="0" value="${g.max_screens ?? ""}"></div>
        <div class="field"><label>AI designs per month</label><input name="ai_monthly_limit" type="number" min="0" value="${g.ai_monthly_limit}"></div>
        <div class="field"><label>&nbsp;</label><label class="chk"><input type="checkbox" name="ai_enabled" ${g.ai_enabled ? "checked" : ""}> AI agent enabled</label></div></div>
        <div class="field"><label>Internal notes</label><textarea name="notes">${esc(g.notes)}</textarea></div>
        ${g.is_own ? "" : `<div class="card" style="background:var(--bg2)"><div class="row"><div class="grow small muted">Deleting removes this organization's users, screens, content, media and reports permanently.</div><button type="button" class="btn sm danger" data-del>Delete organization</button></div></div>`}`,
      onOpen: (f, close) => f.querySelector("[data-del]")?.addEventListener("click", async () => {
        const typed = prompt(`Type the organization name to delete it permanently:\n${g.name}`);
        if (typed !== g.name) { if (typed !== null) toast("Name didn't match — nothing deleted", true); return; }
        await guard(() => api(`/platform/orgs/${g.id}`, { method: "DELETE" }), "Organization deleted");
        close(true);
      }),
      onSubmit: (f) => {
        const v = formData(f);
        return api(`/platform/orgs/${g.id}`, { method: "PATCH", body: {
          name: v.name, plan: v.plan, notes: v.notes, ai_enabled: !!v.ai_enabled,
          ai_monthly_limit: parseInt(v.ai_monthly_limit || "0", 10),
          max_screens: v.max_screens === "" ? null : parseInt(v.max_screens, 10),
        } });
      },
    }).then((ok) => ok && rerender());
  });
}

const stat = (l, n) => `<div class="card stat"><div class="l">${l}</div><div class="n" style="font-size:22px">${n}</div></div>`;
const fmtTokens = (n) => (n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1e3 ? Math.round(n / 1e3) + "k" : String(n));
function tempPassword() {
  const c = "abcdefghjkmnpqrstuvwxyzABCDEFGHJKMNPQRSTUVWXYZ23456789";
  return Array.from(crypto.getRandomValues(new Uint32Array(12)), (x) => c[x % c.length]).join("");
}
