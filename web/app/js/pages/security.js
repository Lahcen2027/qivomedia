import { api, h, esc, ago, modal, confirmDialog, guard, store, can, toast } from "../core.js";

const card = (inner) => h(`<div class="auth"><form class="card" novalidate><div class="brand"><span class="logo"></span>Qivomedia</div>${inner}</form></div>`);

/** Second sign-in step: authenticator code or recovery code. */
export function mfaChallenge(app, ticket, onDone) {
  app.innerHTML = "";
  const c = card(`<h1 style="margin-bottom:4px">Two-factor authentication</h1>
    <p class="muted" style="margin:0 0 20px">Enter the 6-digit code from your authenticator app.</p>
    <div class="field"><label>Code</label><input name="code" inputmode="numeric" autocomplete="one-time-code" required maxlength="11"
      style="font:700 24px ui-monospace,monospace;letter-spacing:.35em;text-align:center" placeholder="123456"></div>
    <button class="btn primary" style="width:100%;justify-content:center;padding:10px">Verify</button>
    <p class="small muted" style="margin:14px 0 0">Lost your phone? Enter one of your recovery codes instead (like <code>abcde-12345</code>).</p>
    <p class="small" style="margin:8px 0 0"><a href="#/login" data-back>Back to sign in</a></p>`);
  app.appendChild(c);
  const f = c.querySelector("form");
  f.code.focus();
  c.querySelector("[data-back]").onclick = () => location.reload();
  f.onsubmit = async (e) => {
    e.preventDefault();
    const btn = f.querySelector("button"); btn.disabled = true;
    try {
      await api("/auth/mfa/verify", { method: "POST", body: { ticket, code: f.code.value } });
      onDone();
    } catch (err) {
      toast(err.message, true);
      btn.disabled = false; f.code.select();
      if (/expired/.test(err.message)) setTimeout(() => location.reload(), 1500);
    }
  };
}

/** Full-page enrollment when the organization requires 2FA. */
export function mfaForcedSetup(app, onDone) {
  app.innerHTML = "";
  const c = card(`<h1 style="margin-bottom:4px">Set up two-factor authentication</h1>
    <p class="muted" style="margin:0 0 16px"><b>${esc(store.me.organization.name)}</b> requires a second step at sign-in to protect its screens. It takes a minute.</p>
    <div data-slot></div>
    <p class="small" style="margin:14px 0 0"><a href="#" data-out>Sign out</a></p>`);
  app.appendChild(c);
  c.querySelector("[data-out]").onclick = (e) => { e.preventDefault(); fetch("/api/auth/logout", { method: "POST" }).then(() => location.reload()); };
  enrollInto(c.querySelector("[data-slot]"), onDone);
}

/** Renders the QR → code → recovery codes flow into `slot`. */
async function enrollInto(slot, onDone) {
  let s;
  try { s = await api("/auth/mfa/setup", { method: "POST" }); } catch (err) { slot.textContent = err.message; return; }
  slot.innerHTML = `<ol class="small" style="padding-left:18px;margin:0 0 14px;line-height:1.7">
      <li>Open an authenticator app: Google Authenticator, Microsoft Authenticator, 1Password, Authy…</li>
      <li>Scan this QR code, or enter the key by hand.</li>
      <li>Type the 6-digit code it shows.</li></ol>
    <div style="display:flex;gap:16px;align-items:center;flex-wrap:wrap;margin-bottom:14px">
      <img src="data:image/png;base64,${s.qr_png_base64}" alt="QR code for your authenticator app" width="168" height="168" style="background:#fff;border-radius:10px;padding:6px">
      <div style="min-width:0;flex:1"><div class="small muted">Key</div><code style="word-break:break-all;font-size:14px">${esc(s.secret.replace(/(.{4})/g, "$1 ").trim())}</code></div>
    </div>
    <div class="field"><label>Code from the app</label><input name="code" inputmode="numeric" autocomplete="one-time-code" maxlength="7"
      style="font:700 22px ui-monospace,monospace;letter-spacing:.3em;text-align:center" placeholder="123456"></div>
    <button type="button" class="btn primary" data-enable style="width:100%;justify-content:center">Turn on two-factor authentication</button>`;
  const input = slot.querySelector("input[name=code]");
  input.focus();
  const go = async () => {
    const btn = slot.querySelector("[data-enable]"); btn.disabled = true;
    try {
      const r = await api("/auth/mfa/enable", { method: "POST", body: { code: input.value } });
      showRecoveryCodes(slot, r.recovery_codes, onDone);
    } catch (err) { toast(err.message, true); btn.disabled = false; input.select(); }
  };
  slot.querySelector("[data-enable]").onclick = go;
  input.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); go(); } };
}

function showRecoveryCodes(slot, codes, onDone) {
  const text = `Qivomedia recovery codes for ${store.me.user.email}\nEach code works once.\n\n${codes.join("\n")}\n`;
  slot.innerHTML = `<div class="card" style="background:rgba(52,211,153,.07);border-color:rgba(52,211,153,.35);margin-bottom:14px"><b>Two-factor authentication is on.</b></div>
    <p class="small" style="margin:0 0 10px"><b>Save these recovery codes</b> somewhere safe (a password manager is ideal). Each one signs you in once if you lose your phone. They won't be shown again.</p>
    <div class="mono" style="display:grid;grid-template-columns:1fr 1fr;gap:6px 18px;background:var(--bg2);border:1px solid var(--line2);border-radius:10px;padding:14px;margin-bottom:12px;font-size:15px">${codes.map((c) => `<span>${esc(c)}</span>`).join("")}</div>
    <div class="row" style="margin-bottom:12px"><button type="button" class="btn sm" data-copy>Copy</button><button type="button" class="btn sm" data-dl>Download .txt</button></div>
    <label class="chk" style="margin-bottom:12px"><input type="checkbox" data-ok> I've saved my recovery codes</label>
    <button type="button" class="btn primary" data-done style="width:100%;justify-content:center" disabled>Continue</button>`;
  slot.querySelector("[data-copy]").onclick = () => navigator.clipboard.writeText(text).then(() => toast("Recovery codes copied"));
  slot.querySelector("[data-dl]").onclick = () => {
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([text], { type: "text/plain" }));
    a.download = "qivomedia-recovery-codes.txt"; a.click();
  };
  const ok = slot.querySelector("[data-ok]"), done = slot.querySelector("[data-done]");
  ok.onchange = () => (done.disabled = !ok.checked);
  done.onclick = onDone;
}

const device = (ua) => {
  if (!ua) return "Unknown device";
  const b = /Edg\//.test(ua) ? "Edge" : /Chrome\//.test(ua) ? "Chrome" : /Firefox\//.test(ua) ? "Firefox" : /Safari\//.test(ua) ? "Safari" : "Browser";
  const os = /Windows/.test(ua) ? "Windows" : /Android/.test(ua) ? "Android" : /iPhone|iPad/.test(ua) ? "iOS" : /Mac OS X/.test(ua) ? "macOS" : /Linux/.test(ua) ? "Linux" : "";
  return os ? `${b} on ${os}` : b;
};

/** Security section on the Team page: own 2FA, own sessions, organization policy. */
export async function securitySection(slot, users, rerenderPage) {
  const [sessions, org] = await Promise.all([api("/auth/sessions"), api("/org/security")]);
  const me = store.me.user;
  const el = h(`<div class="card" style="margin-bottom:18px">
    <h2 style="margin-bottom:14px">Security</h2>
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:18px">
      <div>
        <div class="row" style="margin-bottom:6px"><b class="grow">Two-factor authentication</b>${me.mfa_enabled ? `<span class="badge ok">On</span>` : `<span class="badge warn">Off</span>`}</div>
        <p class="small muted" style="margin:0 0 10px">${me.mfa_enabled ? "Signing in needs your password and a code from your authenticator app." : "Add a code from your phone at sign-in, so a stolen password isn't enough."}</p>
        <div class="row">${me.mfa_enabled
          ? `<button class="btn sm" data-codes>New recovery codes</button>${org.require_mfa ? "" : `<button class="btn sm ghost danger" data-off>Turn off</button>`}`
          : `<button class="btn sm primary" data-on>Set up two-factor authentication</button>`}</div>
      </div>
      <div>
        <div class="row" style="margin-bottom:6px"><b class="grow">Organization policy</b>${org.require_mfa ? `<span class="badge ok">2FA required</span>` : `<span class="badge">2FA optional</span>`}</div>
        <p class="small muted" style="margin:0 0 10px">${org.members_with_mfa} of ${org.members} member${org.members === 1 ? "" : "s"} use two-factor authentication.${org.require_mfa ? " Everyone must set it up at their next sign-in." : ""}</p>
        ${can.admin() ? `<label class="chk"><input type="checkbox" data-require ${org.require_mfa ? "checked" : ""}> Require two-factor authentication for every member</label>` : ""}
      </div>
    </div>
    <div class="row" style="margin:20px 0 8px"><b class="grow">Your active sessions</b>${sessions.length > 1 ? `<button class="btn sm" data-others>Sign out all other sessions</button>` : ""}</div>
    <table><thead><tr><th>Device</th><th>IP address</th><th>Signed in</th><th>Last active</th><th></th></tr></thead><tbody>
      ${sessions.map((s) => `<tr data-sid="${esc(s.id)}"><td>${esc(device(s.user_agent))}${s.current ? ` <span class="badge acc">This device</span>` : ""}${s.mfa ? ` <span class="badge" title="Signed in with two-factor authentication">2FA</span>` : ""}</td>
        <td class="mono">${esc(s.ip || "")}</td><td class="muted">${ago(s.created_at)}</td><td class="muted">${ago(s.last_used_at)}</td>
        <td class="t-actions">${s.current ? "" : `<button class="btn sm ghost danger" data-revoke>Sign out</button>`}</td></tr>`).join("")}
    </tbody></table>
  </div>`);
  slot.replaceChildren(el);
  // Refresh the whole Team page (not just this section) with up-to-date account info.
  const refresh = async () => { store.me = await api("/auth/me"); rerenderPage(); };

  el.querySelector("[data-on]")?.addEventListener("click", () => {
    const body = h(`<div></div>`);
    let finished = false;
    modal({ title: "Set up two-factor authentication", body, submit: "", cancel: "Close", wide: false,
      onOpen: (f, close) => enrollInto(body, () => { finished = true; close(true); }) }).then(() => finished && refresh());
  });
  el.querySelector("[data-off]")?.addEventListener("click", () => modal({
    title: "Turn off two-factor authentication", submit: "Turn off", danger: true,
    body: `<p class="small muted" style="margin-top:0">Your account will be protected by your password only.</p>
      <div class="field"><label>Password</label><input name="password" type="password" required autocomplete="current-password"></div>
      <div class="field"><label>Authenticator or recovery code</label><input name="code" required autocomplete="one-time-code"></div>`,
    onSubmit: (f) => api("/auth/mfa/disable", { method: "POST", body: { password: f.password.value, code: f.code.value } }),
  }).then((ok) => ok && (toast("Two-factor authentication turned off"), refresh())));
  el.querySelector("[data-codes]")?.addEventListener("click", () => {
    const body = h(`<div><p class="small muted" style="margin-top:0">Your old recovery codes stop working.</p>
      <div class="field"><label>Code from your authenticator app</label><input name="code" required autocomplete="one-time-code"></div></div>`);
    modal({ title: "New recovery codes", body, submit: "Generate", onSubmit: async (f) => {
      const r = await api("/auth/mfa/recovery-codes", { method: "POST", body: { code: f.code.value } });
      showRecoveryCodes(body, r.recovery_codes, () => document.querySelector(".modal-bg [data-x]")?.click());
      f.querySelector("footer").remove();
      return false;
    } });
  });
  el.querySelector("[data-require]")?.addEventListener("change", async (e) => {
    try {
      await api("/org/security", { method: "PATCH", body: { require_mfa: e.target.checked } });
      toast(e.target.checked ? "Two-factor authentication is now required for every member" : "Two-factor authentication is now optional");
      refresh();
    } catch (err) { toast(err.message, true); e.target.checked = !e.target.checked; }
  });
  el.querySelector("[data-others]")?.addEventListener("click", async () => {
    await guard(() => api("/auth/sessions/revoke-others", { method: "POST" }), "Signed out of all other sessions");
    refresh();
  });
  el.querySelectorAll("[data-revoke]").forEach((b) => b.onclick = async () => {
    await guard(() => api(`/auth/sessions/${b.closest("tr").dataset.sid}`, { method: "DELETE" }), "Session signed out");
    refresh();
  });
}

/** Admin action on a member who lost their phone. */
export async function resetMemberMfa(u) {
  if (!(await confirmDialog(`Reset two-factor authentication for ${u.name}?`, "Their authenticator and recovery codes stop working and they're signed out. They set it up again at next sign-in.", "Reset 2FA"))) return false;
  await guard(() => api(`/users/${u.id}`, { method: "PATCH", body: { reset_mfa: true } }), "Two-factor authentication reset");
  return true;
}
