// Core helpers shared by all dashboard pages.

export const store = {
  token: localStorage.getItem("qivo.token"),
  me: null,
  cache: {},
};

export const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

export async function api(path, opts = {}) {
  const headers = { ...(opts.headers || {}) };
  if (!(opts.body instanceof FormData) && opts.body !== undefined) headers["Content-Type"] = "application/json";
  if (store.token) headers.Authorization = `Bearer ${store.token}`;
  const res = await fetch(`/api${path}`, {
    ...opts,
    headers,
    body: opts.body === undefined || opts.body instanceof FormData || typeof opts.body === "string" ? opts.body : JSON.stringify(opts.body),
  });
  const body = await res.json().catch(() => ({}));
  if (res.status === 401 && !path.startsWith("/auth/")) {
    logout();
    throw new Error("Session expired");
  }
  if (!res.ok) throw new Error(body.error || `${res.status} ${res.statusText}`);
  return body;
}

export function setToken(t) {
  store.token = t;
  if (t) localStorage.setItem("qivo.token", t);
  else localStorage.removeItem("qivo.token");
}

export function logout() {
  if (store.token) fetch("/api/auth/logout", { method: "POST", headers: { Authorization: `Bearer ${store.token}` } }).catch(() => {});
  setToken(null);
  store.me = null;
  location.hash = "#/login";
  location.reload();
}

export function toast(msg, err = false, ms) {
  const t = document.createElement("div");
  t.className = `toast ${err ? "err" : ""}`;
  t.textContent = msg;
  document.getElementById("toasts").appendChild(t);
  setTimeout(() => t.remove(), ms || (err ? 6000 : 3200));
}

/** Run an async action, toasting any error. */
export async function guard(fn, okMsg) {
  try {
    const r = await fn();
    if (okMsg) toast(okMsg);
    return r;
  } catch (e) {
    toast(e.message, true);
    throw e;
  }
}

/** Parse an HTML string into a single element. */
export function h(html) {
  const t = document.createElement("template");
  t.innerHTML = html.trim();
  return t.content.firstElementChild;
}

export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

/**
 * Modal dialog. `onSubmit(form, close)` may return false to keep it open.
 */
export function modal({ title, body, submit = "Save", wide = false, onSubmit, onOpen, danger = false, cancel = "Cancel" }) {
  return new Promise((resolve) => {
    const bg = h(`<div class="modal-bg"><form class="modal ${wide ? "wide" : ""}" novalidate>
      <header><h2>${esc(title)}</h2><button type="button" class="btn ghost sm" data-x>✕</button></header>
      <div class="body"></div>
      <footer>${cancel ? `<button type="button" class="btn" data-x>${esc(cancel)}</button>` : ""}${submit ? `<button class="btn ${danger ? "danger" : "primary"}" type="submit">${esc(submit)}</button>` : ""}</footer>
    </form></div>`);
    const form = bg.querySelector("form");
    const b = bg.querySelector(".body");
    if (typeof body === "string") b.innerHTML = body; else if (body) b.appendChild(body);
    const close = (v) => { bg.remove(); document.removeEventListener("keydown", onKey); resolve(v); };
    const onKey = (e) => { if (e.key === "Escape") close(null); };
    document.addEventListener("keydown", onKey);
    bg.addEventListener("mousedown", (e) => { if (e.target === bg) close(null); });
    bg.querySelectorAll("[data-x]").forEach((x) => x.onclick = () => close(null));
    form.onsubmit = async (e) => {
      e.preventDefault();
      const btn = form.querySelector("button[type=submit]");
      if (btn) btn.disabled = true;
      try {
        const r = onSubmit ? await onSubmit(form, close) : true;
        if (r !== false) close(r ?? true);
      } catch (err) {
        toast(err.message, true);
      } finally {
        if (btn) btn.disabled = false;
      }
    };
    document.body.appendChild(bg);
    onOpen?.(form, close);
    form.querySelector("input:not([type=hidden]),select,textarea")?.focus();
  });
}

export function confirmDialog(title, text, submit = "Delete") {
  return modal({ title, body: `<p class="muted" style="margin:0">${esc(text)}</p>`, submit, danger: true });
}

export const formData = (form) => Object.fromEntries(new FormData(form).entries());

export function fmtBytes(n) {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i ? 1 : 0)} ${u[i]}`;
}

export function fmtDur(sec) {
  sec = Math.round(sec);
  const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
  return h ? `${h}h ${m}m` : m ? `${m}m ${String(s).padStart(2, "0")}s` : `${s}s`;
}

export function ago(iso) {
  if (!iso) return "never";
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

export const can = {
  edit: () => ["owner", "admin", "editor"].includes(store.me?.user.role),
  admin: () => ["owner", "admin"].includes(store.me?.user.role),
};

/** Load (and memoize per page render) a collection. */
export async function load(name, fresh = false) {
  if (!fresh && store.cache[name]) return store.cache[name];
  store.cache[name] = await api(`/${name}`);
  return store.cache[name];
}
export const invalidate = (...names) => names.forEach((n) => delete store.cache[n]);

export const ICON = {
  home: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M3 11l9-7 9 7v9a1 1 0 01-1 1h-5v-6H9v6H4a1 1 0 01-1-1z"/></svg>',
  display: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="2" y="4" width="20" height="13" rx="2"/><path d="M8 21h8M12 17v4"/></svg>',
  media: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><path d="M21 15l-5-5L5 21"/></svg>',
  playlist: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M3 6h13M3 12h13M3 18h8M18 15v6l4-3z"/></svg>',
  layout: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18M9 9v12"/></svg>',
  calendar: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="4" width="18" height="17" rx="2"/><path d="M16 2v4M8 2v4M3 10h18"/></svg>',
  bolt: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M13 2L4 14h7l-1 8 9-12h-7z"/></svg>',
  chart: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M3 3v18h18M8 17V10M13 17V6M18 17v-4"/></svg>',
  siren: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M10.3 3.9L1.8 18a2 2 0 001.7 3h17a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z"/><path d="M12 9v4M12 17h.01"/></svg>',
  shield: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/><path d="M9 12l2 2 4-4"/></svg>',
  sparkle: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8zM19 15l.9 2.1L22 18l-2.1.9L19 21l-.9-2.1L16 18l2.1-.9z"/></svg>',
  users: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="9" cy="8" r="4"/><path d="M2 21a7 7 0 0114 0M17 11a4 4 0 000-8M22 21a7 7 0 00-5-6.7"/></svg>',
};
