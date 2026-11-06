import { api, h, esc, fmtBytes, ago, load, invalidate } from "../core.js";

export async function render(root) {
  invalidate("displays", "layouts");
  const [o, displays, layouts] = await Promise.all([api("/reports/overview"), load("displays"), load("layouts")]);
  const lname = (id) => layouts.find((l) => l.id === id)?.name || "—";
  const steps = [
    [o.media > 0, "Upload media", "Images, videos, PDFs or HTML into the library.", "#/media"],
    [o.playlists > 0, "Build a playlist", "Order items and widgets, set durations.", "#/playlists"],
    [o.layouts > 0, "Design a layout", "Split the screen into zones and assign playlists.", "#/layouts"],
    [o.displays > 0, "Pair a screen", `Open <code>${esc(location.origin)}/player</code> on the screen and enter its code.`, "#/displays"],
    [o.schedules > 0, "Schedule it", "Day-part content by time, day and priority.", "#/schedules"],
  ];
  const done = steps.filter((s) => s[0]).length;

  root.appendChild(h(`<div>
    <div class="head"><div><h1>Overview</h1><p>Everything happening across your screens.</p></div>
      <a class="btn primary" href="/player" target="_blank">Open web player ↗</a></div>
    <div class="grid g4" style="margin-bottom:18px">
      ${stat("Screens online", `${o.online}<span class="muted" style="font-size:18px"> / ${o.displays}</span>`)}
      ${stat("Plays (24h)", o.plays_24h.toLocaleString())}
      ${stat("Active schedules", o.schedules)}
      ${stat("Library", `${o.media} <span class="muted" style="font-size:16px">files · ${fmtBytes(o.storage_bytes)}</span>`)}
    </div>
    ${done < steps.length ? `<div class="card" style="margin-bottom:18px">
      <div class="row" style="margin-bottom:12px"><h2 class="grow">Get your first screen live</h2><span class="badge acc">${done}/${steps.length}</span></div>
      <div class="grid g3">${steps.map(([ok, t, d, href], i) => `
        <a href="${href}" class="card" style="background:var(--bg2);display:flex;gap:12px;color:inherit">
          <span class="badge ${ok ? "ok" : ""}" style="height:24px">${ok ? "✓" : i + 1}</span>
          <div><div style="font-weight:600">${t}</div><div class="muted small">${d}</div></div></a>`).join("")}</div></div>` : ""}
    <div class="card flush">
      <div class="row" style="padding:14px 16px"><h2 class="grow">Screens</h2><a href="#/displays" class="small">Manage →</a></div>
      ${displays.length ? `<table><thead><tr><th>Screen</th><th>Status</th><th>Playing</th><th>Last seen</th></tr></thead><tbody>
        ${displays.map((d) => `<tr data-id="${d.id}"><td><b>${esc(d.name)}</b></td>
          <td>${d.status === "disabled" ? `<span class="badge warn">Disabled</span>` : d.online ? `<span class="badge ok"><span class="dot on"></span>Online</span>` : `<span class="badge bad">Offline</span>`}</td>
          <td class="muted playing">${esc(lname(d.default_layout_id))}</td><td class="muted">${ago(d.last_seen)}</td></tr>`).join("")}
      </tbody></table>` : `<div class="empty"><h3>No screens yet</h3><p>Pair one from <a href="#/displays">Displays</a>.</p></div>`}
    </div>
  </div>`));

  const onLive = (e) => {
    const m = e.detail;
    if (!root.isConnected) return window.removeEventListener("qivo:live", onLive);
    if (m.type === "display_status") {
      const cell = root.querySelector(`tr[data-id="${m.display_id}"] .playing`);
      if (cell) cell.textContent = lname(m.layout_id);
    }
    if (m.type === "display_presence") { window.removeEventListener("qivo:live", onLive); render((root.innerHTML = "", root)); }
  };
  window.addEventListener("qivo:live", onLive);
}

const stat = (l, n) => `<div class="card stat"><div class="l">${l}</div><div class="n">${n}</div></div>`;
