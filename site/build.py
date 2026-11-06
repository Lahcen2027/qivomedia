#!/usr/bin/env python3
"""Build the Qivomedia website into site/dist/ (English at /, French at /fr/, Arabic at /ar/).

Usage:  python3 site/build.py
Env:    QIVO_SITE_URL (default https://qivomedia.com)   — canonical/hreflang links
        QIVO_APP_URL  (default https://app.qivomedia.com) — "Sign in" links

Edit copy in site/src/i18n.json (prices are the *_price keys), layout in site/src/page.html.
"""
import html, json, os, re, shutil, time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SRC, DIST = ROOT / "src", ROOT / "dist"
SITE_URL = os.environ.get("QIVO_SITE_URL", "https://qivomedia.com").rstrip("/")
APP_URL = os.environ.get("QIVO_APP_URL", "https://app.qivomedia.com").rstrip("/")
LANGS = {"en": "", "fr": "fr/", "ar": "ar/"}
LABELS = {"en": "EN", "fr": "FR", "ar": "العربية"}

# Fire-alert text for the hero demo button (same wording as the server's alert template).
FIRE = {
    "fr": ("Alerte incendie", "Évacuez immédiatement le bâtiment par la sortie la plus proche. N'utilisez pas les ascenseurs."),
    "ar": ("إنذار حريق", "غادروا المبنى فوراً عبر أقرب مخرج. لا تستعملوا المصاعد."),
    "en": ("Fire alarm", "Leave the building now by the nearest exit. Do not use the lifts."),
}


def build():
    tpl = (SRC / "page.html").read_text(encoding="utf-8")
    i18n = json.loads((SRC / "i18n.json").read_text(encoding="utf-8"))
    keys = set(re.findall(r"\{\{(\w+)\}\}", tpl))
    stamp = str(int(time.time()))
    if DIST.exists():
        shutil.rmtree(DIST)
    (DIST / "assets").mkdir(parents=True)
    shutil.copy(SRC / "site.css", DIST / "assets" / "site.css")
    shutil.copy(SRC / "site.js", DIST / "assets" / "site.js")

    for lang, path in LANGS.items():
        t = i18n[lang]
        order = [lang] + [l for l in ("fr", "ar", "en") if l != lang]
        alert = [{"lang": l, "title": FIRE[l][0], "message": FIRE[l][1]} for l in order]
        raw = {  # already-safe HTML / generated values
            "lang": lang, "build": stamp, "root": "/", "lang_path": path,
            "base_url": SITE_URL, "canonical": f"{SITE_URL}/{path}", "app_url": APP_URL,
            "lang_links": " · ".join(
                f'<a href="/{p}" hreflang="{l}" lang="{l}"' + (' aria-current="page"' if l == lang else "") + f">{LABELS[l]}</a>"
                for l, p in LANGS.items()),
            "sectors": "".join(f"<span>{html.escape(s)}</span>" for s in t["sectors"]),
            "week_days": "".join(f'<div class="d">{html.escape(d)}</div>' for d in t["week_days"]),
            "alert_json": json.dumps(alert, ensure_ascii=False).replace("</", "<\\/"),
        }
        out = tpl
        missing = []
        for k in keys:
            if k in raw:
                v = raw[k]
            elif k in t and isinstance(t[k], str):
                v = html.escape(t[k], quote=True)
            else:
                missing.append(k)
                continue
            out = out.replace("{{" + k + "}}", v)
        if missing:
            raise SystemExit(f"[{lang}] missing translations: {', '.join(sorted(missing))}")
        dest = DIST / path / "index.html"
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(out, encoding="utf-8")
        print(f"built /{path}index.html")

    (DIST / "robots.txt").write_text(f"User-agent: *\nAllow: /\nSitemap: {SITE_URL}/sitemap.xml\n")
    urls = "".join(f"<url><loc>{SITE_URL}/{p}</loc></url>" for p in LANGS.values())
    (DIST / "sitemap.xml").write_text(f'<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{urls}</urlset>\n')


if __name__ == "__main__":
    build()
