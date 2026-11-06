// Setup and "connecting" screens bundled inside the player app.
const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
const $ = (id) => document.getElementById(id);

const T = {
  en: {},
  fr: {
    connecting_title: "Connexion…", connecting_to: "En attente de", change_server: "Changer de serveur",
    setup_title: "Configurer cet écran", setup_lead: "Saisissez l'adresse de votre serveur Qivomedia. L'écran affichera ensuite un code à saisir dans le tableau de bord.",
    server: "Adresse du serveur", test: "Tester", restart: "Redémarrage quotidien", restart_help: "Garde l'écran fluide. Laissez vide pour jamais.",
    autostart: "Démarrer automatiquement avec l'ordinateur", save: "Enregistrer et démarrer", keys: "Ctrl+Maj+F10 ouvre cet écran depuis le lecteur.", quit: "Quitter le lecteur",
    ok: "Serveur joignable ✓", unreachable: "Serveur injoignable — vérifiez l'adresse et le réseau.", retry: "Nouvel essai dans 10 s…",
  },
};
const lang = (navigator.language || "en").slice(0, 2) === "fr" ? "fr" : "en";
const t = (k, fallback) => T[lang][k] || fallback;
document.documentElement.lang = lang;
document.querySelectorAll("[data-t]").forEach((n) => { if (T[lang][n.dataset.t]) n.textContent = T[lang][n.dataset.t]; });

// Tell the watchdog this screen is alive (otherwise it would restart a healthy setup screen).
invoke("heartbeat"); setInterval(() => invoke("heartbeat"), 15000);

async function main() {
  const cfg = await invoke("get_config");
  invoke("device_info").then((d) => {
    const m = (d.monitors || []).find((x) => x.primary) || d.monitors?.[0];
    $("device").textContent = `v${cfg.version} · ${d.hostname} · ${m ? `${m.width}×${m.height}${m.hz ? ` @ ${Math.round(m.hz)} Hz` : ""}` : ""}`;
  }).catch(() => { $("device").textContent = `v${cfg.version}`; });
  $("server").value = cfg.server_url || "";
  $("restart").value = cfg.restart_at || "";
  $("autostart").checked = cfg.autostart !== false;

  if (location.hash === "#connecting" && cfg.server_url) connecting(cfg.server_url);
  else $("setup").hidden = false;
}

async function connecting(server) {
  $("connecting").hidden = false;
  $("cserver").textContent = server;
  for (;;) {
    const r = await invoke("test_server", { url: server }).catch(() => ({ ok: false }));
    if (r.ok) { await invoke("save_config", { serverUrl: server }); return; }
    $("cstatus").textContent = t("retry", "Not reachable yet — retrying in 10 s…");
    await new Promise((res) => setTimeout(res, 10000));
    if ($("connecting").hidden) return;
  }
}

$("change").onclick = () => { $("connecting").hidden = true; $("setup").hidden = false; $("server").focus(); };

$("test").onclick = async () => {
  const out = $("result");
  out.className = "small muted"; out.textContent = "…";
  try {
    const r = await invoke("test_server", { url: $("server").value });
    out.className = `small ${r.ok ? "ok" : "bad"}`;
    out.textContent = r.ok ? `${t("ok", "Server reachable ✓")} (${r.origin})` : t("unreachable", "Server not reachable — check the address and the network.");
  } catch (e) { out.className = "small bad"; out.textContent = String(e); }
};

$("form").onsubmit = async (e) => {
  e.preventDefault();
  try {
    await invoke("save_config", { serverUrl: $("server").value, restartAt: $("restart").value, autostart: $("autostart").checked });
  } catch (err) { $("result").className = "small bad"; $("result").textContent = String(err); }
};

$("quit").onclick = () => invoke("quit_player");
main();
