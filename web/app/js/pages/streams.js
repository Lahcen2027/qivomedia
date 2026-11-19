// Media → Live streams: IP cameras, encoders and IPTV channels shown on screens.
import { api, h, esc, modal, confirmDialog, guard, can, toast } from "../core.js";

const EXAMPLES = [
  ["IP camera (RTSP)", "rtsp://user:password@192.168.1.20:554/stream1"],
  ["IPTV multicast", "udp://@239.10.10.1:1234"],
  ["Encoder (SRT)", "srt://10.0.0.8:9000"],
  ["Encoder (RTMP)", "rtmp://10.0.0.8/live/channel1"],
  ["HLS channel", "https://example.com/live/channel.m3u8"],
];

export async function streamsSection(slot) {
  const streams = await api("/streams");
  const el = h(`<div>
    <div class="row" style="margin-bottom:12px">
      <p class="grow muted small" style="margin:0">Show IP cameras, encoders and IPTV channels in any zone of a layout.
        The server relays them so every screen can play them: H.264 is passed through as it is, and H.265 or MPEG-2 sources are converted live.
        A relay runs only while a screen is watching.</p>
      ${can.admin() ? `<button class="btn primary" data-add>+ Add stream</button>` : ""}
    </div>
    ${streams.length ? `<table><thead><tr><th>Stream</th><th>Source</th><th>Video</th><th>Status</th><th></th></tr></thead><tbody>
      ${streams.map((s) => `<tr data-id="${esc(s.id)}">
        <td><b>${esc(s.name)}</b>${s.relay ? "" : ` <span class="badge">direct</span>`}</td>
        <td class="small mono" style="max-width:320px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${esc(s.source)}">${esc(s.source)}</td>
        <td class="small">${s.video_codec ? `${esc(s.video_codec.toUpperCase())}${s.resolution ? ` · ${esc(s.resolution)}` : ""}${s.relay && s.video_codec !== "h264" && s.transcode !== "never" ? " → H.264" : ""}` : `<span class="muted">not tested</span>`}</td>
        <td class="small">${s.status === "live" ? `<span class="live-dot"></span>live` : s.status === "starting" ? "starting…" : `<span class="muted">idle</span>`}
          ${s.last_error ? `<div class="small" style="color:var(--bad);max-width:260px" title="${esc(s.last_error)}">${esc(s.last_error.slice(0, 70))}</div>` : ""}</td>
        <td class="t-actions">${s.relay ? `<button class="btn sm" data-preview>Preview</button>` : ""}
          ${can.edit() ? `<button class="btn sm" data-test>Test</button>` : ""}
          ${can.admin() ? `<button class="btn sm" data-edit>Edit</button> <button class="btn sm ghost danger" data-del>✕</button>` : ""}</td></tr>`).join("")}
      </tbody></table>` : `<div class="card empty"><h3>No live streams yet</h3><p>Add an IP camera, an encoder or an IPTV channel, then add it to a playlist with <b>● Live stream</b>.</p></div>`}
  </div>`);
  slot.replaceChildren(el);
  const reload = () => streamsSection(slot);
  const byRow = (b) => streams.find((s) => s.id === b.closest("tr").dataset.id);

  el.querySelector("[data-add]")?.addEventListener("click", () => editStream(null).then((ok) => ok && reload()));
  el.querySelectorAll("[data-edit]").forEach((b) => b.onclick = () => editStream(byRow(b)).then((ok) => ok && reload()));
  el.querySelectorAll("[data-del]").forEach((b) => b.onclick = async () => {
    const s = byRow(b);
    if (!(await confirmDialog(`Delete ${s.name}?`, "Screens showing it will skip it."))) return;
    await guard(() => api(`/streams/${s.id}`, { method: "DELETE" }), "Stream deleted");
    reload();
  });
  el.querySelectorAll("[data-test]").forEach((b) => b.onclick = async () => {
    const s = byRow(b);
    b.disabled = true; b.textContent = "Testing…";
    try {
      const r = await api(`/streams/${s.id}/test`, { method: "POST" });
      if (r.ok) toast(`${s.name}: ${r.video_codec?.toUpperCase()} ${r.resolution || ""}. ${r.note}`, false, 8000);
      else toast(`${s.name}: ${r.error}`, true, 10000);
    } catch (e) { toast(e.message, true); }
    reload();
  });
  el.querySelectorAll("[data-preview]").forEach((b) => b.onclick = () => preview(byRow(b)));
}

function editStream(s) {
  return modal({
    title: s ? `Edit ${s.name}` : "Add a live stream", wide: true, submit: s ? "Save" : "Add",
    body: `<div class="field"><label>Name</label><input name="name" required value="${esc(s?.name || "")}" placeholder="Main entrance camera"></div>
      <div class="field"><label>Stream address</label><input name="source" ${s ? "" : "required"} spellcheck="false" class="mono" placeholder="${s ? esc(s.source) + " (unchanged)" : "rtsp://user:password@192.168.1.20:554/stream1"}">
        <div class="small faint" style="margin-top:6px">Examples: ${EXAMPLES.map(([l, u]) => `<a href="#" data-ex="${esc(u)}" title="${esc(u)}">${l}</a>`).join(" · ")}.
        Camera passwords are stored encrypted. Ask the camera's manual for its RTSP address.</div></div>
      <div class="fields2">
        <div class="field"><label>Video conversion</label><select name="transcode">
          ${[["auto", "Automatic (convert unless H.264)"], ["always", "Always convert to H.264"], ["never", "Never (pass through)"]].map(([v, l]) => `<option value="${v}" ${v === (s?.transcode || "auto") ? "selected" : ""}>${l}</option>`).join("")}</select></div>
        <div class="field"><label class="chk" style="margin-top:28px"><input type="checkbox" name="relay" ${s?.relay === false ? "" : "checked"}> Relay through the server</label>
          <div class="small faint">Required for cameras and encoders. Untick only for public HLS channels that screens can reach directly.</div></div>
      </div>`,
    onOpen: (f) => f.querySelectorAll("[data-ex]").forEach((a) => a.onclick = (e) => { e.preventDefault(); f.source.value = a.dataset.ex; f.source.focus(); }),
    onSubmit: async (f) => {
      const body = { name: f.name.value, transcode: f.transcode.value, relay: f.relay.checked };
      if (f.source.value.trim()) body.source = f.source.value.trim();
      const saved = await api(s ? `/streams/${s.id}` : "/streams", { method: s ? "PUT" : "POST", body });
      // Check the connection straight away, so problems show up now rather than on a screen.
      const t = await api(`/streams/${saved.id}/test`, { method: "POST" }).catch(() => null);
      if (t?.ok) toast(`${saved.name} connected: ${t.video_codec?.toUpperCase()} ${t.resolution || ""}`, false, 7000);
      else if (t) toast(`${saved.name} was saved, but the source didn't answer: ${t.error}`, true, 10000);
    },
  });
}

async function preview(s) {
  let hls = null;
  const done = modal({
    title: `${s.name} — preview`, wide: true, submit: "Close", cancel: "",
    body: `<div class="stream-preview"><video muted autoplay playsinline controls></video></div>
      <p class="small muted" data-msg style="margin:8px 0 0">Connecting… (the relay starts now, which takes a few seconds)</p>`,
    onOpen: async (f) => {
      const v = f.querySelector("video");
      v.addEventListener("playing", () => { f.querySelector("[data-msg]").textContent = "Live. This is what screens receive."; }, { once: true });
      if (!window.Hls) await new Promise((res, rej) => { const sc = document.createElement("script"); sc.src = "/player/vendor/hls.light.min.js"; sc.onload = res; sc.onerror = rej; document.head.appendChild(sc); });
      if (window.Hls?.isSupported()) {
        hls = new window.Hls({ enableWorker: false, liveSyncDurationCount: 2 });
        hls.on(window.Hls.Events.ERROR, (_, d) => { if (d.fatal) f.querySelector("[data-msg]").textContent = `Not playing: ${d.details}. Use Test to see what the source says.`; });
        hls.loadSource(s.play_url); hls.attachMedia(v);
      } else { v.src = s.play_url; }
    },
  });
  await done;
  hls?.destroy();
}
