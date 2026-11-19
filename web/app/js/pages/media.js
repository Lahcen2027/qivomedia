import { api, h, esc, fmtBytes, fmtDur, modal, confirmDialog, guard, load, invalidate, can, store, toast } from "../core.js";
import { streamsSection } from "./streams.js";

export function thumb(m) {
  if (!m) return `<div class="thumb">missing</div>`;
  if (m.status === "processing") return `<div class="thumb converting"><div><div class="spin"></div><b data-pct>${m.progress ? m.progress + "%" : "Converting…"}</b><div class="small">making it playable on every screen</div></div></div>`;
  if (m.status === "failed") return `<div class="thumb failed" title="${esc(m.error || "")}"><div><b>Conversion failed</b><div class="small">${esc((m.error || "").slice(0, 90))}</div></div></div>`;
  if (m.kind === "image") return `<div class="thumb"><img loading="lazy" src="${m.url}" alt=""></div>`;
  if (m.kind === "video") return `<div class="thumb"><video src="${m.url}#t=1" muted preload="metadata"></video></div>`;
  if (m.kind === "pdf" && m.pages?.length) return `<div class="thumb"><img class="page" loading="lazy" src="${m.pages[0]}" alt=""><span class="thumb-badge">${m.pages.length} page${m.pages.length > 1 ? "s" : ""}</span></div>`;
  if (m.kind === "audio") return `<div class="thumb">♪ audio</div>`;
  return `<div class="thumb">${esc(m.kind)}</div>`;
}

const res = (m) => m.width && m.height ? ` · ${m.width}×${m.height}${Math.max(m.width, m.height) >= 3840 ? " (4K)" : ""}` : "";

function videoDuration(file) {
  return new Promise((res) => {
    if (!file.type.startsWith("video/") && !file.type.startsWith("audio/")) return res(null);
    const v = document.createElement("video");
    v.preload = "metadata";
    v.onloadedmetadata = () => { URL.revokeObjectURL(v.src); res(isFinite(v.duration) ? Math.round(v.duration * 1000) : null); };
    v.onerror = () => res(null);
    v.src = URL.createObjectURL(file);
  });
}

function uploadOne(file, onProgress) {
  return new Promise(async (resolve, reject) => {
    const fd = new FormData();
    const d = await videoDuration(file);
    if (d) fd.append("duration_ms", String(d));
    fd.append("file", file, file.name);
    const xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/media");
    xhr.withCredentials = true;
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded / e.total);
    xhr.onload = () => {
      const body = JSON.parse(xhr.responseText || "{}");
      xhr.status < 300 ? resolve(body) : reject(new Error(body.error || `Upload failed (${xhr.status})`));
    };
    xhr.onerror = () => reject(new Error("Network error during upload"));
    xhr.send(fd);
  });
}

export async function render(root) {
  invalidate("media");
  const media = await load("media");
  let filter = "all", q = "";
  const page = h(`<div>
    <div class="head"><div><h1>Media library</h1><p>Images, video, audio, PDF and HTML. Formats screens can't play (AVI, MPEG-2, VOB, WMV, HEVC…) are converted automatically, and PDFs become page-by-page slides.</p></div>
      ${can.edit() ? `<label class="btn primary" style="margin:0" data-upload-btn>+ Upload<input type="file" multiple hidden></label>` : ""}</div>
    <div class="tabs" style="margin-bottom:14px"><a data-view="files" class="active">Files</a><a data-view="live">● Live streams</a></div>
    <div data-live-slot hidden></div>
    <div data-files>
    ${can.edit() ? `<div class="drop">Drop files here to upload</div><div class="uploads stack" style="margin-bottom:14px"></div>` : ""}
    <div class="row" style="margin-bottom:14px">
      <input placeholder="Search…" style="max-width:260px" data-q>
      <div class="tabs" style="margin:0;border:0">${["all", "image", "video", "audio", "pdf", "html", "other"].map((k) => `<a data-k="${k}" class="${k === "all" ? "active" : ""}">${k}</a>`).join("")}</div>
    </div>
    <div class="media-grid"></div>
  </div></div>`);
  root.appendChild(page);
  const grid = page.querySelector(".media-grid");

  const draw = () => {
    const list = media.filter((m) => (filter === "all" || m.kind === filter) && m.name.toLowerCase().includes(q));
    grid.innerHTML = list.length ? "" : `<div class="empty" style="grid-column:1/-1"><h3>${media.length ? "No matches" : "Your library is empty"}</h3><p>${media.length ? "" : "Drag files in to get started."}</p></div>`;
    for (const m of list) {
      const t = h(`<div class="tile" data-mid="${esc(m.id)}">${thumb(m)}
        <div class="meta"><div class="name" title="${esc(m.name)}">${esc(m.name)}</div>
        <div class="small muted">${esc(m.kind)} · ${fmtBytes(m.size)}${m.duration_ms ? " · " + fmtDur(m.duration_ms / 1000) : ""}${res(m)}</div>
        ${m.error && m.status === "ready" ? `<div class="small" style="color:var(--warn)">${esc(m.error)}</div>` : ""}</div>
        ${can.edit() ? `<div class="acts"><button class="btn sm" data-r>Rename</button><button class="btn sm danger" data-d>✕</button></div>` : ""}</div>`);
      t.querySelector("[data-r]")?.addEventListener("click", () => modal({
        title: "Rename media", body: `<div class="field"><label>Name</label><input name="name" value="${esc(m.name)}"></div>`,
        onSubmit: async (f) => { Object.assign(m, await api(`/media/${m.id}`, { method: "PATCH", body: { name: f.name.value } })); draw(); },
      }));
      t.querySelector("[data-d]")?.addEventListener("click", async () => {
        if (!(await confirmDialog("Delete media?", m.name))) return;
        await guard(() => api(`/media/${m.id}`, { method: "DELETE" }), "Deleted");
        media.splice(media.indexOf(m), 1); draw();
      });
      grid.appendChild(t);
    }
  };
  draw();

  // Conversion progress, live from the server.
  const onLive = async (e) => {
    const d = e.detail;
    if (d.type !== "media_progress") return;
    if (!page.isConnected) return window.removeEventListener("qivo:live", onLive);
    const m = media.find((x) => x.id === d.id);
    if (!m) return;
    if (d.status === "processing") {
      m.progress = d.progress;
      const pct = grid.querySelector(`[data-mid="${m.id}"] [data-pct]`);
      if (pct) pct.textContent = `${d.progress}%`;
      return;
    }
    invalidate("media");
    const fresh = (await api("/media")).find((x) => x.id === d.id);
    if (fresh) Object.assign(m, fresh);
    draw();
    if (d.status === "ready") toast(`${m.name} is ready to play`);
    else toast(`${m.name}: conversion failed`, true, 8000);
  };
  window.addEventListener("qivo:live", onLive);

  page.querySelectorAll("[data-view]").forEach((a) => a.onclick = () => {
    page.querySelectorAll("[data-view]").forEach((x) => x.classList.toggle("active", x === a));
    const live = a.dataset.view === "live";
    page.querySelector("[data-files]").hidden = live;
    page.querySelector("[data-live-slot]").hidden = !live;
    const up = page.querySelector("[data-upload-btn]");
    if (up) up.style.display = live ? "none" : "";
    if (live) streamsSection(page.querySelector("[data-live-slot]")).catch((err) => toast(err.message, true));
  });
  if (location.hash.includes("live")) page.querySelector('[data-view="live"]').click();

  page.querySelector("[data-q]").oninput = (e) => { q = e.target.value.toLowerCase(); draw(); };
  page.querySelectorAll("[data-k]").forEach((a) => a.onclick = () => {
    page.querySelectorAll("[data-k]").forEach((x) => x.classList.remove("active"));
    a.classList.add("active"); filter = a.dataset.k; draw();
  });

  const ups = page.querySelector(".uploads");
  const handle = async (files) => {
    for (const f of files) {
      const row = h(`<div class="hbar"><span class="small" style="width:220px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(f.name)}</span><div class="bar"><div style="width:0"></div></div><span class="small muted pct">0%</span></div>`);
      ups.appendChild(row);
      try {
        const created = await uploadOne(f, (p) => { row.querySelector(".bar div").style.width = `${p * 100}%`; row.querySelector(".pct").textContent = `${Math.round(p * 100)}%`; });
        media.unshift(...created);
        row.remove();
        draw();
      } catch (e) {
        row.querySelector(".pct").textContent = "failed";
        toast(`${f.name}: ${e.message}`, true);
      }
    }
    invalidate("media");
  };
  page.querySelector("input[type=file]")?.addEventListener("change", (e) => handle([...e.target.files]));
  const drop = page.querySelector(".drop");
  if (drop) {
    const main = document.querySelector(".main");
    const over = (e) => { e.preventDefault(); drop.classList.add("over"); };
    main.ondragover = over;
    main.ondragleave = () => drop.classList.remove("over");
    main.ondrop = (e) => { e.preventDefault(); drop.classList.remove("over"); if (root.isConnected) handle([...e.dataTransfer.files]); };
  }
}

/** Media picker used by the playlist editor. Resolves to an array of media. */
export async function pickMedia() {
  invalidate("media");
  // Items still converting can be added (they start playing once ready); failed ones can't.
  const media = (await load("media")).filter((m) => m.status !== "failed");
  const chosen = new Set();
  const body = h(`<div><div class="media-grid">${media.length ? media.map((m) => `<div class="tile pick" data-id="${m.id}">${thumb(m)}<div class="meta"><div class="name">${esc(m.name)}</div><div class="small muted">${m.kind}${m.status === "processing" ? " · converting" : m.pages?.length ? ` · ${m.pages.length} pages` : ""}</div></div></div>`).join("") : `<div class="empty" style="grid-column:1/-1">No media yet — upload some in the Media library.</div>`}</div></div>`);
  body.querySelectorAll(".tile").forEach((t) => t.onclick = () => {
    const id = t.dataset.id;
    chosen.has(id) ? chosen.delete(id) : chosen.add(id);
    t.style.borderColor = chosen.has(id) ? "var(--accent)" : "";
    t.style.boxShadow = chosen.has(id) ? "0 0 0 3px rgba(124,92,255,.3)" : "";
  });
  const r = await modal({ title: "Add media", body, wide: true, submit: "Add selected", onSubmit: () => media.filter((m) => chosen.has(m.id)) });
  return Array.isArray(r) ? r : [];
}
