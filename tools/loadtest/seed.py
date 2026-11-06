#!/usr/bin/env python3
"""Seed a fresh qivo-server with a realistic campus and N paired screens.

Usage: seed.py BASE_URL DB_PATH N KEYS_OUT
Creates: owner account, 20 images, 3 playlists, 3 layouts, 4 schedules,
10 building groups, and N active screens spread across the groups.
Screens are inserted straight into SQLite (registration is rate-limited per IP
on purpose); everything else goes through the real API.
"""
import hashlib, io, json, secrets, sqlite3, sys, uuid
from datetime import datetime, timezone

import requests
from PIL import Image

base, db_path, n, keys_out = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
EMAIL, PASSWORD = "owner@campus.test", "stress test campus 2026"
s = requests.Session()
s.headers["Origin"] = base

r = s.post(f"{base}/api/auth/signup", json={"org_name": "Load Test University", "name": "Owner", "email": EMAIL, "password": PASSWORD})
if r.status_code != 200:
    r = s.post(f"{base}/api/auth/login", json={"email": EMAIL, "password": PASSWORD})
r.raise_for_status()

def api(method, path, body=None):
    r = s.request(method, base + path, json=body)
    if r.status_code >= 400:
        raise SystemExit(f"{method} {path}: {r.status_code} {r.text}")
    return r.json()

media = []
for i in range(20):
    buf = io.BytesIO()
    Image.new("RGB", (1920, 1080), (i * 12 % 255, 80, 160)).save(buf, "JPEG", quality=70)
    r = s.post(f"{base}/api/media", files={"file": (f"slide-{i}.jpg", buf.getvalue(), "image/jpeg")})
    r.raise_for_status()
    j = r.json()
    media.append((j[0] if isinstance(j, list) else j)["id"])

def items(ids):
    return [{"type": "media", "media_id": m, "duration": 8} for m in ids] + [
        {"type": "text", "text": "Welcome to campus", "duration": 6},
        {"type": "clock", "duration": 5},
    ]

pl_main = api("POST", "/api/playlists", {"name": "Campus loop", "items": items(media[:12])})["id"]
pl_news = api("POST", "/api/playlists", {"name": "News", "items": items(media[12:])})["id"]
pl_tick = api("POST", "/api/playlists", {"name": "Ticker", "items": [{"type": "ticker", "text": "Library open until 22:00 · Exams start Monday", "duration": 30}]})["id"]

def layout(name, pl):
    return api("POST", "/api/layouts", {"name": name, "regions": [
        {"name": "Main", "x": 0, "y": 0, "w": 75, "h": 89, "playlist_id": pl},
        {"name": "Side", "x": 75, "y": 0, "w": 25, "h": 89, "playlist_id": pl_news},
        {"name": "Ticker", "x": 0, "y": 89, "w": 100, "h": 11, "playlist_id": pl_tick},
    ]})["id"]

l1, l2, l3 = layout("Day", pl_main), layout("Evening", pl_news), layout("Weekend", pl_main)
groups = [api("POST", "/api/groups", {"name": f"Building {chr(65 + g)}"})["id"] for g in range(10)]
api("POST", "/api/schedules", {"name": "Day", "layout_id": l1, "target_type": "all", "time_from": "07:00", "time_to": "18:00", "priority": 1})
api("POST", "/api/schedules", {"name": "Evening", "layout_id": l2, "target_type": "all", "time_from": "18:00", "time_to": "07:00", "priority": 1})
api("POST", "/api/schedules", {"name": "Weekend", "layout_id": l3, "target_type": "all", "days_mask": 0b1000001, "priority": 2})
api("POST", "/api/schedules", {"name": "Building A promo", "layout_id": l2, "target_type": "group", "target_id": groups[0], "priority": 3})

org = api("GET", "/api/auth/me")
org_id = org.get("org_id") or org.get("org", {}).get("id") or org.get("user", {}).get("org_id")
db = sqlite3.connect(db_path, timeout=30)
if not org_id:
    org_id = db.execute("SELECT id FROM organizations LIMIT 1").fetchone()[0]
now = datetime.now(timezone.utc).isoformat()
keys, rows = [], []
for i in range(n):
    k = secrets.token_urlsafe(36)
    keys.append(k)
    rows.append((uuid.uuid4().hex, org_id, f"Screen {i:05d}", hashlib.sha256(k.encode()).hexdigest(), "active", groups[i % 10], l1, now))
db.executemany("INSERT INTO displays (id, org_id, name, device_key_hash, status, group_id, default_layout_id, created_at) VALUES (?,?,?,?,?,?,?,?)", rows)
db.commit()
open(keys_out, "w").write("\n".join(keys) + "\n")
print(json.dumps({"org_id": org_id, "screens": n, "email": EMAIL, "password": PASSWORD}))
