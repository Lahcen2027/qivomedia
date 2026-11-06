import { api, store, logout, h, esc, toast, ICON, formData, can } from "./core.js";
import { mfaChallenge, mfaForcedSetup } from "./pages/security.js";
import * as overview from "./pages/overview.js";
import * as displays from "./pages/displays.js";
import * as media from "./pages/media.js";
import * as playlists from "./pages/playlists.js";
import * as layouts from "./pages/layouts.js";
import * as schedules from "./pages/schedules.js";
import * as triggers from "./pages/triggers.js";
import * as reports from "./pages/reports.js";
import * as team from "./pages/team.js";
import * as platform from "./pages/platform.js";
import * as alerts from "./pages/alerts.js";

const app = document.getElementById("app");

const ROUTES = [
  [/^\/$/, overview],
  [/^\/displays$/, displays],
  [/^\/media$/, media],
  [/^\/playlists$/, playlists],
  [/^\/playlists\/([\w-]+)$/, playlists, "edit"],
  [/^\/layouts$/, layouts],
  [/^\/layouts\/([\w-]+)$/, layouts, "edit"],
  [/^\/schedules$/, schedules],
  [/^\/triggers$/, triggers],
  [/^\/reports$/, reports],
  [/^\/team$/, team],
  [/^\/platform$/, platform],
  [/^\/alerts$/, alerts],
];

const NAV = [
  ["", "Overview", ICON.home, "/"],
  ["sec", "Screens"],
  ["", "Emergency alerts", ICON.siren, "/alerts"],
  ["", "Displays", ICON.display, "/displays"],
  ["", "Schedule", ICON.calendar, "/schedules"],
  ["", "Triggers", ICON.bolt, "/triggers"],
  ["sec", "Content"],
  ["", "Media library", ICON.media, "/media"],
  ["", "Playlists", ICON.playlist, "/playlists"],
  ["", "Layouts", ICON.layout, "/layouts"],
  ["sec", "Insights"],
  ["", "Proof of play", ICON.chart, "/reports"],
  ["", "Team & audit", ICON.users, "/team"],
];

// ---------- auth screens ----------
async function authScreen() {
  const st = await api("/auth/status");
  if (store.me) return; // signed in while this was loading
  const signup = st.needs_setup || location.hash === "#/signup";
  app.innerHTML = "";
  const card = h(`<div class="auth"><form class="card">
    <div class="brand"><span class="logo"></span>Qivomedia</div>
    <h1 style="margin-bottom:4px">${st.needs_setup ? "Set up your workspace" : signup ? "Create a workspace" : "Sign in"}</h1>
    <p class="muted" style="margin:0 0 20px">${st.needs_setup ? "Create the first organization and owner account." : signup ? "A new organization with you as owner." : "Digital signage, done properly."}</p>
    ${signup ? `<div class="field"><label>Organization</label><input name="org_name" required placeholder="Acme Retail"></div>
                <div class="field"><label>Your name</label><input name="name" required></div>` : ""}
    <div class="field"><label>Email</label><input name="email" type="email" required autocomplete="username"></div>
    <div class="field"><label>Password</label><input name="password" type="password" required ${signup ? 'minlength="12"' : ""} autocomplete="${signup ? "new-password" : "current-password"}">${signup ? `<div class="small faint" style="margin-top:4px">At least 12 characters. A short phrase of 3–4 words is strong and easy to remember.</div>` : ""}</div>
    <button class="btn primary" style="width:100%;justify-content:center;padding:10px">${signup ? "Create workspace" : "Sign in"}</button>
    ${!signup ? `<p class="small" style="text-align:center;margin:14px 0 0"><a href="#/forgot">Forgot your password?</a></p>` : ""}
    ${!st.needs_setup && st.allow_signup ? `<p class="small muted" style="text-align:center;margin:10px 0 0">${signup ? `<a href="#/login">Have an account? Sign in</a>` : `<a href="#/signup">Create a new workspace</a>`}</p>` : ""}
  </form></div>`);
  app.appendChild(card);
  card.querySelector("form").onsubmit = async (e) => {
    e.preventDefault();
    const btn = e.target.querySelector("button");
    btn.disabled = true;
    try {
      const r = await api(signup ? "/auth/signup" : "/auth/login", { method: "POST", body: formData(e.target) });
      if (r.mfa_required) return mfaChallenge(app, r.ticket, () => { location.hash = "#/"; start(); });
      location.hash = "#/";
      start();
    } catch (err) {
      toast(err.message === "authentication required" ? "Wrong email or password" : err.message, true, 9000);
      btn.disabled = false;
    }
  };
  card.querySelector("input").focus();
}

function authCard(inner) {
  app.innerHTML = "";
  const c = h(`<div class="auth"><form class="card"><div class="brand"><span class="logo"></span>Qivomedia</div>${inner}</form></div>`);
  app.appendChild(c);
  return c.querySelector("form");
}

function forgotScreen() {
  const f = authCard(`<h1 style="margin-bottom:4px">Reset your password</h1>
    <p class="muted" style="margin:0 0 20px">Enter your account email. We'll send you a link to choose a new password.</p>
    <div class="field"><label>Email</label><input name="email" type="email" required autocomplete="username"></div>
    <button class="btn primary" style="width:100%;justify-content:center;padding:10px">Send reset link</button>
    <p class="small" style="text-align:center;margin:14px 0 0"><a href="#/login">Back to sign in</a></p>`);
  f.email.focus();
  f.onsubmit = async (e) => {
    e.preventDefault();
    const btn = f.querySelector("button"); btn.disabled = true;
    try {
      await api("/auth/forgot", { method: "POST", body: { email: f.email.value } });
      f.innerHTML = `<div class="brand"><span class="logo"></span>Qivomedia</div><h1 style="margin-bottom:8px">Check your email</h1>
        <p class="muted" style="margin:0 0 18px">If an account exists for <b>${esc(f.email?.value || "")}</b>, a reset link is on its way. It's valid for 1 hour.</p>
        <p class="small muted">No email after a few minutes? Check spam, or ask your administrator to reset your password from Team.</p>
        <p class="small" style="margin:14px 0 0"><a href="#/login">Back to sign in</a></p>`;
    } catch (err) { toast(err.message, true); btn.disabled = false; }
  };
}

async function resetScreen() {
  const token = new URLSearchParams(location.hash.split("?")[1] || "").get("token") || "";
  let info;
  try { info = await api("/auth/token-info", { method: "POST", body: { token } }); }
  catch (err) {
    authCard(`<h1 style="margin-bottom:8px">Link expired</h1><p class="muted" style="margin:0 0 18px">${esc(err.message)}</p>
      <a class="btn primary" href="#/forgot" style="width:100%;justify-content:center">Send a new link</a>`);
    return;
  }
  const invite = info.purpose === "invite";
  const f = authCard(`<h1 style="margin-bottom:4px">${invite ? `Welcome, ${esc(info.name)}` : "Choose a new password"}</h1>
    <p class="muted" style="margin:0 0 20px">${invite ? "Choose a password to finish setting up your account" : "For"} <b>${esc(info.email)}</b>.</p>
    <div class="field"><label>New password</label><input name="password" type="password" minlength="12" required autocomplete="new-password"><div class="small faint" style="margin-top:4px">At least 12 characters. A short phrase of 3–4 words works well.</div></div>
    <div class="field"><label>Repeat password</label><input name="password2" type="password" minlength="12" required autocomplete="new-password"></div>
    <button class="btn primary" style="width:100%;justify-content:center;padding:10px">${invite ? "Create my account" : "Save password and sign in"}</button>`);
  f.password.focus();
  f.onsubmit = async (e) => {
    e.preventDefault();
    if (f.password.value.length < 12) return toast("Use at least 12 characters", true);
    if (f.password.value !== f.password2.value) return toast("The two passwords don't match", true);
    const btn = f.querySelector("button"); btn.disabled = true;
    try {
      const r = await api("/auth/reset", { method: "POST", body: { token, password: f.password.value } });
      history.replaceState(null, "", location.pathname + "#/");
      if (r.signin_required) {
        toast("Password changed — sign in with your new password and your authenticator code", false, 7000);
        return authScreen();
      }
      toast(invite ? "Welcome to Qivomedia" : "Password changed — you're signed in");
      start();
    } catch (err) { toast(err.message, true); btn.disabled = false; }
  };
}

// ---------- shell ----------
function shell() {
  app.innerHTML = "";
  const s = h(`<div class="shell">
    <aside class="side">
      <div class="brand"><span class="logo"></span>Qivomedia</div>
      <nav class="nav">${[...NAV, ...(store.me.user.platform_admin ? [["sec", "Platform"], ["", "Customers & usage", ICON.shield, "/platform"]] : [])].map(([k, label, icon, path]) => k === "sec" ? `<div class="sec">${label}</div>` : `<a href="#${path}" data-path="${path}">${icon}<span>${label}</span></a>`).join("")}</nav>
      <div class="foot">
        <div style="font-weight:600">${esc(store.me.organization.name)}</div>
        <div class="muted">${esc(store.me.user.email)} · ${esc(store.me.user.role)}</div>
        <a href="#" id="logout" class="small">Sign out</a>
      </div>
    </aside>
    <main class="main"><div id="alertbar" hidden></div><div class="page" id="page"></div></main>
  </div>`);
  app.appendChild(s);
  s.querySelector("#logout").onclick = (e) => { e.preventDefault(); logout(); };
}

let currentPage = null;
async function route() {
  if (!store.me) return;
  const path = location.hash.replace(/^#/, "") || "/";
  if (path === "/login" || path === "/signup") { location.hash = "#/"; return; }
  document.querySelectorAll(".nav a").forEach((a) => {
    const p = a.dataset.path;
    a.classList.toggle("active", p === "/" ? path === "/" : path.startsWith(p));
  });
  const root = document.getElementById("page");
  for (const [re, mod, fn] of ROUTES) {
    const m = path.match(re);
    if (m) {
      currentPage = mod;
      root.innerHTML = "";
      try {
        await (fn ? mod[fn] : mod.render)(root, ...m.slice(1));
      } catch (e) {
        console.error(e);
        root.innerHTML = `<div class="empty"><h3>Something went wrong</h3><p>${esc(e.message)}</p></div>`;
      }
      return;
    }
  }
  root.innerHTML = `<div class="empty"><h3>Not found</h3><a href="#/">Back to overview</a></div>`;
}

// ---------- live events ----------
let liveWs;
function live() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  liveWs = new WebSocket(`${proto}://${location.host}/ws/admin`); // authenticated by the session cookie
  liveWs.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    window.dispatchEvent(new CustomEvent("qivo:live", { detail: msg }));
    if (msg.type === "trigger_fired") toast(`Trigger fired on ${msg.displays} screen(s)`);
    if (msg.type === "alert_started" || msg.type === "alert_cleared") alerts.refreshAlertBar();
  };
  liveWs.onclose = () => setTimeout(() => store.me && live(), 3000);
}

let booting = null;
function start() {
  // One boot at a time: a hash change during sign-in must not re-show the login screen.
  booting = boot().finally(() => { booting = null; });
  return booting;
}

async function boot() {
  const hash = location.hash;
  if (hash.startsWith("#/reset")) return resetScreen();
  if (hash.startsWith("#/forgot")) return forgotScreen();
  try {
    store.me = await api("/auth/me");
  } catch {
    store.me = null;
    return authScreen();
  }
  if (store.me.user.mfa_setup_required) return mfaForcedSetup(app, () => start());
  shell();
  live();
  route();
  alerts.refreshAlertBar();
}

window.addEventListener("hashchange", () => {
  if (booting) return;
  if (location.hash.startsWith("#/reset") || (!store.me && location.hash.startsWith("#/forgot"))) return start();
  return store.me ? route() : authScreen();
});
start();

export { can };
