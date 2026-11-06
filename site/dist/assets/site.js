// Qivomedia website: drives the live player in the hero and sends demo requests.
(function () {
  const frame = document.querySelector(".tv iframe");
  const alertLines = JSON.parse(document.getElementById("i18n-alert").textContent);

  document.querySelectorAll("[data-demo]").forEach((b) => b.addEventListener("click", () => {
    if (!frame || !frame.contentWindow) return;
    const msg = b.dataset.demo === "alert"
      ? { type: "qivo-demo", action: "alert", alert: { kind: "fire", color: "red", lines: alertLines } }
      : { type: "qivo-demo", action: "clear" };
    frame.contentWindow.postMessage(msg, location.origin);
  }));

  const form = document.getElementById("demoform");
  if (!form) return;
  const out = form.querySelector(".formmsg");
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const data = Object.fromEntries(new FormData(form).entries());
    const emailOk = /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(data.email || "");
    if (!data.name.trim() || !emailOk) {
      out.className = "formmsg err"; out.textContent = form.dataset.invalid;
      (data.name.trim() ? form.email : form.name).focus();
      return;
    }
    const btn = form.querySelector("button[type=submit]");
    btn.disabled = true;
    try {
      const res = await fetch("/api/public/demo-requests", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(data) });
      if (!res.ok) throw new Error((await res.json().catch(() => ({}))).error || res.statusText);
      form.reset();
      out.className = "formmsg ok"; out.textContent = form.dataset.ok;
    } catch (err) {
      out.className = "formmsg err"; out.textContent = form.dataset.err;
    } finally {
      btn.disabled = false;
    }
  });
})();
