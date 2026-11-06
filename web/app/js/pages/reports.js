import { api, h, esc, fmtDur } from "../core.js";

export async function render(root, days = 7) {
  const from = new Date(Date.now() - days * 864e5).toISOString();
  const r = await api(`/reports/pop?from=${encodeURIComponent(from)}`);
  const maxDay = Math.max(1, ...r.by_day.map((d) => d.plays));
  const maxMedia = Math.max(1, ...r.by_media.map((d) => d.plays));
  const total = r.by_day.reduce((a, d) => a + d.plays, 0);
  const secs = r.by_day.reduce((a, d) => a + d.seconds, 0);

  const page = h(`<div>
    <div class="head"><div><h1>Proof of play</h1><p>Every item each screen actually showed, recorded on the device (even offline) and synced.</p></div>
      <div class="row"><select data-range style="width:auto">${[1, 7, 30, 90].map((d) => `<option value="${d}" ${d === days ? "selected" : ""}>Last ${d === 1 ? "24 hours" : d + " days"}</option>`).join("")}</select>
      <button class="btn" data-csv>Export CSV</button></div></div>
    <div class="grid g4" style="margin-bottom:18px">
      <div class="card stat"><div class="l">Plays</div><div class="n">${total.toLocaleString()}</div></div>
      <div class="card stat"><div class="l">Airtime</div><div class="n">${fmtDur(secs)}</div></div>
      <div class="card stat"><div class="l">Screens reporting</div><div class="n">${r.by_display.length}</div></div>
      <div class="card stat"><div class="l">Distinct items</div><div class="n">${r.by_media.length}</div></div>
    </div>
    <div class="card" style="margin-bottom:18px"><h2 style="margin-bottom:14px">Plays per day</h2>
      ${r.by_day.length ? `<div style="display:flex;align-items:flex-end;gap:6px;height:160px">${r.by_day.map((d) => `<div title="${d.day}: ${d.plays} plays" style="flex:1;display:flex;flex-direction:column;align-items:center;gap:4px;height:100%;justify-content:flex-end">
        <div class="small muted">${d.plays}</div><div style="width:100%;max-width:48px;height:${(d.plays / maxDay) * 120}px;border-radius:6px 6px 2px 2px;background:linear-gradient(180deg,var(--accent2),var(--accent))"></div><div class="small faint">${d.day.slice(5)}</div></div>`).join("")}</div>`
        : `<div class="empty">No plays recorded in this period yet.</div>`}
    </div>
    <div class="grid" style="grid-template-columns:3fr 2fr">
      <div class="card flush"><div style="padding:14px 16px"><h2>Top content</h2></div><table><thead><tr><th>Item</th><th>Plays</th><th>Airtime</th></tr></thead><tbody>
        ${r.by_media.map((m) => `<tr><td>${esc(m.name || (m.item_type === "media" ? "Deleted media" : m.item_type + " widget"))}<div class="bar" style="margin-top:6px"><div style="width:${(m.plays / maxMedia) * 100}%"></div></div></td><td>${m.plays}</td><td>${fmtDur(m.seconds)}</td></tr>`).join("") || `<tr><td colspan="3" class="muted">—</td></tr>`}</tbody></table></div>
      <div class="card flush"><div style="padding:14px 16px"><h2>By screen</h2></div><table><thead><tr><th>Screen</th><th>Plays</th><th>Airtime</th></tr></thead><tbody>
        ${r.by_display.map((d) => `<tr><td>${esc(d.name || "Removed screen")}</td><td>${d.plays}</td><td>${fmtDur(d.seconds)}</td></tr>`).join("") || `<tr><td colspan="3" class="muted">—</td></tr>`}</tbody></table></div>
    </div></div>`);
  root.appendChild(page);
  page.querySelector("[data-range]").onchange = (e) => { root.innerHTML = ""; render(root, parseInt(e.target.value, 10)); };
  page.querySelector("[data-csv]").onclick = () => {
    const rows = [["item", "type", "plays", "airtime_seconds"], ...r.by_media.map((m) => [m.name || "", m.item_type, m.plays, m.seconds])];
    const csv = rows.map((row) => row.map((c) => `"${String(c).replace(/"/g, '""')}"`).join(",")).join("\n");
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([csv], { type: "text/csv" }));
    a.download = `proof-of-play-${days}d.csv`;
    a.click();
  };
}
