import { api, h, esc, ago, modal, confirmDialog, guard, formData, can, store, toast } from "../core.js";
import { securitySection, resetMemberMfa } from "./security.js";
import { ssoSection } from "./sso.js";

export async function render(root) {
  const users = await api("/users");
  const audit = can.admin() ? await api("/audit") : [];
  const name = (id) => users.find((u) => u.id === id)?.name || "system";
  const rerender = () => { root.innerHTML = ""; render(root); };
  const roleBadge = (r) => `<span class="badge ${r === "owner" ? "acc" : r === "admin" ? "ok" : ""}">${r}</span>`;

  const page = h(`<div>
    <div class="head"><div><h1>Team & audit</h1><p>Role-based access: <b>viewer</b> (read-only) · <b>editor</b> (content & schedules) · <b>admin</b> (screens & users) · <b>owner</b>.</p></div>
      <div class="row"><button class="btn" data-chpw>Change my password</button>${can.admin() ? `<button class="btn primary" data-new>+ Add member</button>` : ""}</div></div>
    <div data-sec></div>
    ${can.admin() ? `<div data-sso></div>` : ""}
    <div class="card flush" style="margin-bottom:18px"><table><thead><tr><th>Name</th><th>Email</th><th>Role</th><th>2FA</th><th>Joined</th><th></th></tr></thead><tbody>
      ${users.map((u) => `<tr data-id="${u.id}"><td><b>${esc(u.name)}</b>${u.id === store.me.user.id ? ` <span class="faint small">(you)</span>` : ""}</td><td>${esc(u.email)}</td><td>${roleBadge(u.role)}</td>
        <td>${u.mfa_enabled ? `<span class="badge ok">On</span>` : `<span class="badge">Off</span>`}</td>
        <td class="muted">${ago(u.created_at)}</td><td class="t-actions">${can.admin() && u.mfa_enabled && u.id !== store.me.user.id ? `<button class="btn sm ghost" data-mfa title="For a member who lost their phone">Reset 2FA</button> ` : ""}${can.admin() && u.role !== "owner" && u.id !== store.me.user.id ? `<select data-role style="width:auto;padding:4px 8px">${["admin", "editor", "viewer"].map((r) => `<option ${r === u.role ? "selected" : ""}>${r}</option>`).join("")}</select> <button class="btn sm ghost danger" data-d>Remove</button>` : ""}</td></tr>`).join("")}
    </tbody></table></div>
    ${can.admin() ? `<div class="card flush"><div style="padding:14px 16px"><h2>Audit log</h2></div><table><thead><tr><th>When</th><th>Who</th><th>Action</th><th>Detail</th></tr></thead><tbody>
      ${audit.map((a) => `<tr><td class="muted small">${new Date(a.created_at).toLocaleString()}</td><td>${esc(name(a.user_id))}</td><td><code>${esc(a.action)}</code></td><td>${esc(a.detail)}</td></tr>`).join("") || `<tr><td colspan="4" class="muted">No activity yet.</td></tr>`}
    </tbody></table></div>` : ""}</div>`);
  root.appendChild(page);

  page.querySelector("[data-new]")?.addEventListener("click", () => modal({
    title: "Add team member", submit: "Add",
    body: `<div class="fields2"><div class="field"><label>Name</label><input name="name" required></div><div class="field"><label>Email</label><input name="email" type="email" required></div></div>
      <div class="field"><label class="chk"><input type="checkbox" name="invite" checked> Email them an invitation — they choose their own password</label></div>
      <div class="fields2"><div class="field" data-pw><label>Temporary password (12+ characters)</label><input name="password" type="text" minlength="12"></div>
      <div class="field"><label>Role</label><select name="role"><option>editor</option><option>viewer</option><option>admin</option></select></div></div>`,
    onOpen: (f) => {
      const sync = () => { const inv = f.invite.checked; f.querySelector("[data-pw]").style.visibility = inv ? "hidden" : "visible"; f.password.required = !inv; };
      f.invite.onchange = sync; sync();
    },
    onSubmit: async (f) => {
      const v = formData(f);
      await api("/users", { method: "POST", body: { ...v, invite: !!v.invite } });
      toast(v.invite ? `Invitation sent to ${v.email}` : `${v.email} added — share the temporary password with them`);
    },
  }).then((ok) => ok && rerender()));
  page.querySelector("[data-chpw]").onclick = () => modal({
    title: "Change password",
    body: `<div class="field"><label>New password</label><input name="password" type="password" minlength="12" required autocomplete="new-password">
      <div class="small faint" style="margin-top:4px">At least 12 characters. Your other sessions will be signed out.</div></div>`,
    onSubmit: async (f) => { await api(`/users/${store.me.user.id}`, { method: "PATCH", body: formData(f) }); toast("Password changed"); },
  });
  securitySection(page.querySelector("[data-sec]"), users, rerender).catch((e) => toast(e.message, true));
  if (can.admin()) ssoSection(page.querySelector("[data-sso]")).catch((e) => toast(e.message, true));
  page.querySelectorAll("tr[data-id]").forEach((tr) => {
    const id = tr.dataset.id;
    tr.querySelector("[data-mfa]")?.addEventListener("click", async () => { if (await resetMemberMfa(users.find((u) => u.id === id))) rerender(); });
    tr.querySelector("[data-role]")?.addEventListener("change", (e) => guard(() => api(`/users/${id}`, { method: "PATCH", body: { role: e.target.value } }), "Role updated"));
    tr.querySelector("[data-d]")?.addEventListener("click", async () => {
      if (!(await confirmDialog("Remove member?", "They lose access immediately."))) return;
      await guard(() => api(`/users/${id}`, { method: "DELETE" }), "Removed");
      rerender();
    });
  });
}
