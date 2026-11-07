// Team page → "Single sign-on": connect the organization's Microsoft 365, Google Workspace
// or other OpenID Connect provider so people sign in with their university account.
import { api, h, esc, modal, confirmDialog, guard, toast } from "../core.js";

const KINDS = {
  microsoft: { label: "Microsoft 365 (Entra ID)", short: "Microsoft" },
  google: { label: "Google Workspace", short: "Google" },
  oidc: { label: "Other provider (OpenID Connect)", short: "OpenID Connect" },
};
const ROLES = ["viewer", "editor", "admin"];

export async function ssoSection(slot) {
  const d = await api("/sso/providers");
  const el = h(`<div class="card" style="margin-bottom:18px">
    <div class="row" style="margin-bottom:6px"><h2 class="grow" style="margin:0">Single sign-on</h2>
      ${d.sso_required ? `<span class="badge ok">Required</span>` : d.providers.length ? `<span class="badge acc">On</span>` : `<span class="badge">Not set up</span>`}</div>
    <p class="small muted" style="margin:0 0 14px">People sign in with their university account, and new staff get access automatically. Their account and multi-factor sign-in are managed by your IT department.</p>
    ${d.providers.length ? `<table><thead><tr><th>Provider</th><th>Email domains</th><th>New accounts</th><th>Roles from groups</th><th></th></tr></thead><tbody>
      ${d.providers.map((p) => `<tr data-id="${esc(p.id)}">
        <td><b>${esc(p.name)}</b> <span class="badge">${KINDS[p.kind].short}</span>${p.enabled ? "" : ` <span class="badge warn">Off</span>`}</td>
        <td class="small">${p.domains.map(esc).join(", ") || `<span class="muted">any</span>`}</td>
        <td class="small">${p.auto_create ? `Created as <b>${esc(p.default_role)}</b>` : "Invited members only"}</td>
        <td class="small">${p.group_roles.length ? `${p.group_roles.length} rule${p.group_roles.length > 1 ? "s" : ""}` : `<span class="muted">—</span>`}</td>
        <td class="t-actions"><button class="btn sm" data-edit>Edit</button> <button class="btn sm ghost danger" data-del>Remove</button></td></tr>`).join("")}
      </tbody></table>` : ""}
    <div class="row" style="margin-top:12px;flex-wrap:wrap;gap:8px">
      <button class="btn sm${d.providers.length ? "" : " primary"}" data-add="microsoft">+ Microsoft 365</button>
      <button class="btn sm" data-add="google">+ Google Workspace</button>
      <button class="btn sm ghost" data-add="oidc">+ Other provider</button>
    </div>
    <div class="card" style="background:var(--bg2);margin-top:14px;padding:12px 14px">
      <div class="small muted" style="margin-bottom:4px">Redirect URI: give this to your IT department when they register Qivomedia</div>
      <div class="row"><code class="grow mono small" style="word-break:break-all">${esc(d.redirect_uri)}</code><button class="btn sm" data-copy>Copy</button></div>
    </div>
    ${d.providers.length ? `<label class="chk" style="margin-top:14px"><input type="checkbox" data-required ${d.sso_required ? "checked" : ""}>
      Require single sign-on for members. Owners and admins keep their password as an emergency way in.</label>` : ""}
  </div>`);
  slot.replaceChildren(el);
  const reload = () => ssoSection(slot);

  el.querySelector("[data-copy]").onclick = () => navigator.clipboard.writeText(d.redirect_uri).then(() => toast("Redirect URI copied"), () => toast("Select the address and copy it", true));
  el.querySelectorAll("[data-add]").forEach((b) => b.onclick = () => editProvider(b.dataset.add, null, d.redirect_uri).then((ok) => ok && reload()));
  el.querySelectorAll("[data-edit]").forEach((b) => b.onclick = () => {
    const p = d.providers.find((x) => x.id === b.closest("tr").dataset.id);
    editProvider(p.kind, p, d.redirect_uri).then((ok) => ok && reload());
  });
  el.querySelectorAll("[data-del]").forEach((b) => b.onclick = async () => {
    const p = d.providers.find((x) => x.id === b.closest("tr").dataset.id);
    if (!(await confirmDialog(`Remove ${p.name}?`, "People who signed in with it will need a password (or another provider) to sign in.", "Remove"))) return;
    await guard(() => api(`/sso/providers/${p.id}`, { method: "DELETE" }), "Provider removed");
    reload();
  });
  el.querySelector("[data-required]")?.addEventListener("change", async (e) => {
    try {
      await api("/sso/policy", { method: "PUT", body: { sso_required: e.target.checked } });
      toast(e.target.checked ? "Members now sign in with single sign-on only" : "Members can sign in with a password again");
      reload();
    } catch (err) { toast(err.message, true); e.target.checked = !e.target.checked; }
  });
}

const rulesToText = (rules) => (rules || []).map((r) => `${r.group} = ${r.role}`).join("\n");
function textToRules(text) {
  return text.split("\n").map((l) => l.trim()).filter(Boolean).map((l) => {
    const i = l.lastIndexOf("=");
    if (i < 0) throw new Error(`"${l}": write group = role`);
    const role = l.slice(i + 1).trim().toLowerCase();
    if (!ROLES.includes(role)) throw new Error(`"${l}": the role must be viewer, editor or admin`);
    return { group: l.slice(0, i).trim(), role };
  });
}

function editProvider(kind, p, redirectUri) {
  const k = KINDS[kind];
  const help = {
    microsoft: `In the <b>Microsoft Entra admin center</b> → App registrations → New registration. Redirect URI (Web): <code class="mono">${esc(redirectUri)}</code>. Then copy the IDs from the app's Overview page and create a secret under <i>Certificates &amp; secrets</i>.`,
    google: `In <b>Google Cloud console</b> → APIs &amp; Services → Credentials → Create OAuth client ID (Web application). Authorized redirect URI: <code class="mono">${esc(redirectUri)}</code>. Set the consent screen to <i>Internal</i>.`,
    oidc: `Register a confidential client (authorization code flow) with redirect URI <code class="mono">${esc(redirectUri)}</code>.`,
  }[kind];
  const groups = kind !== "google";
  return modal({
    title: p ? `Edit ${p.name}` : `Connect ${k.label}`, wide: true, submit: p ? "Save" : "Connect",
    body: `<p class="small muted" style="margin-top:0">${help}</p>
      <div class="grid" style="grid-template-columns:1fr 1fr;gap:0 16px">
        <div class="field"><label>Button label</label><input name="name" required value="${esc(p?.name || "")}" placeholder="Université Mohammed V"></div>
        ${kind === "microsoft" ? `<div class="field"><label>Directory (tenant) ID</label><input name="tenant" ${p ? "" : "required"} placeholder="${p ? "unchanged" : "00000000-0000-0000-0000-000000000000"}" spellcheck="false"></div>` : ""}
        ${kind === "oidc" ? `<div class="field"><label>Issuer URL</label><input name="issuer" ${p ? "" : "required"} placeholder="${esc(p?.issuer || "https://sso.univ.ma/realms/staff")}" spellcheck="false"></div>` : ""}
        ${kind === "google" ? `<div></div>` : ""}
        <div class="field"><label>${kind === "microsoft" ? "Application (client) ID" : "Client ID"}</label><input name="client_id" required value="${esc(p?.client_id || "")}" spellcheck="false"></div>
        <div class="field"><label>Client secret</label><input name="client_secret" type="password" ${p ? `placeholder="unchanged"` : "required"} autocomplete="off"></div>
        <div class="field"><label>Allowed email domains${kind === "google" ? "" : " (recommended)"}</label><input name="domains" ${kind === "google" ? "required" : ""} value="${esc((p?.domains || []).join(", "))}" placeholder="univ.ma, fs.univ.ma">
          <div class="small faint" style="margin-top:4px">Also lets the login page find this provider from the email address.</div></div>
        <div class="field"><label>Role for new accounts</label><select name="default_role">${ROLES.map((r) => `<option ${r === (p?.default_role || "viewer") ? "selected" : ""}>${r}</option>`).join("")}</select>
          <label class="chk" style="margin-top:8px"><input type="checkbox" name="auto_create" ${p?.auto_create === false ? "" : "checked"}> Create an account the first time someone signs in</label></div>
      </div>
      ${groups ? `<div class="field"><label>Roles from groups (optional): one rule per line, <span class="mono">group = role</span></label>
        <textarea name="group_roles" rows="3" class="mono" placeholder="${kind === "microsoft" ? "3f2a…-object-id-of-Communication-group = editor\n9b1c…-object-id-of-IT-group = admin" : "communication = editor\nit-admins = admin"}">${esc(rulesToText(p?.group_roles))}</textarea>
        <div class="small faint" style="margin-top:4px">${kind === "microsoft" ? "Use each group's <b>Object ID</b>, and add a <i>groups</i> claim under Token configuration in the app registration." : "Group names as your provider sends them in the <i>groups</i> claim."} The highest matching role applies at every sign-in, and people in no listed group get the role for new accounts. Leave empty to manage roles in Qivomedia.</div></div>` : ""}
      ${p ? `<label class="chk"><input type="checkbox" name="enabled" ${p.enabled ? "checked" : ""}> Enabled</label>` : ""}`,
    onSubmit: async (f) => {
      const body = {
        kind, name: f.name.value, client_id: f.client_id.value, client_secret: f.client_secret.value,
        domains: f.domains.value, default_role: f.default_role.value, auto_create: f.auto_create.checked,
      };
      if (f.tenant?.value) body.tenant = f.tenant.value;
      if (f.issuer?.value) body.issuer = f.issuer.value;
      if (groups) body.group_roles = textToRules(f.group_roles.value);
      if (p) body.enabled = f.enabled.checked;
      await api(p ? `/sso/providers/${p.id}` : "/sso/providers", { method: p ? "PUT" : "POST", body });
      toast(p ? "Saved" : `${k.short} sign-in connected. Try it from a private window before requiring it.`, false, 7000);
    },
  });
}
