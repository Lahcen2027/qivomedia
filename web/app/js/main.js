import { api, store, setToken, logout, h, esc, toast, ICON, formData, can } from "./core.js";
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
];

const NAV = [
  ["", "Overview", ICON.home, "/"],
  ["sec", "Screens"],
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
  const signup = st.needs_setup || location.hash === "#/signup";
  app.innerHTML = "";
  const card = h(`<div class="auth"><form class="card">
    <div class="brand"><span class="logo"></span>Qivomedia</div>
    <h1 style="margin-bottom:4px">${st.needs_setup ? "Set up your workspace" : signup ? "Create a workspace" : "Sign in"}</h1>
    <p class="muted" style="margin:0 0 20px">${st.needs_setup ? "Create the first organization and owner account." : signup ? "A new organization with you as owner." : "Digital signage, done properly."}</p>
    ${signup ? `<div class="field"><label>Organization</label><input name="org_name" required placeholder="Acme Retail"></div>
                <div class="field"><label>Your name</label><input name="name" required></div>` : ""}
    <div class="field"><label>Email</label><input name="email" type="email" required autocomplete="username"></div>
    <div class="field"><label>Password</label><input name="password" type="password" required minlength="8" autocomplete="${signup ? "new-password" : "current-password"}"></div>
    <button class="btn primary" style="width:100%;justify-content:center;padding:10px">${signup ? "Create workspace" : "Sign in"}</button>
    ${!st.needs_setup && st.allow_signup ? `<p class="small muted" style="text-align:center;margin:14px 0 0">${signup ? `<a href="#/login">Have an account? Sign in</a>` : `<a href="#/signup">Create a new workspace</a>`}</p>` : ""}
  </form></div>`);
  app.appendChild(card);
  card.querySelector("form").onsubmit = async (e) => {
    e.preventDefault();
    const btn = e.target.querySelector("button");
    btn.disabled = true;
    try {
      const r = await api(signup ? "/auth/signup" : "/auth/login", { method: "POST", body: formData(e.target) });
      setToken(r.token);
      location.hash = "#/";
      start();
    } catch (err) {
      toast(err.message === "authentication required" ? "Wrong email or password" : err.message, true);
      btn.disabled = false;
    }
  };
  card.querySelector("input").focus();
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
    <main class="main"><div class="page" id="page"></div></main>
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
  liveWs = new WebSocket(`${proto}://${location.host}/ws/admin?token=${encodeURIComponent(store.token)}`);
  liveWs.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    window.dispatchEvent(new CustomEvent("qivo:live", { detail: msg }));
    if (msg.type === "trigger_fired") toast(`Trigger fired on ${msg.displays} screen(s)`);
  };
  liveWs.onclose = () => setTimeout(() => store.token && live(), 3000);
}

async function start() {
  if (!store.token) return authScreen();
  try {
    store.me = await api("/auth/me");
  } catch {
    setToken(null);
    return authScreen();
  }
  shell();
  live();
  route();
}

window.addEventListener("hashchange", () => (store.me ? route() : authScreen()));
start();

export { can };
