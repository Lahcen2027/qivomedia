# Stress test — 6 November 2026

**Question:** can one small server run a whole university campus?

**Answer:** yes. A 2-vCPU, 8 GB machine held **15,000 screens** connected at once. It delivered an emergency alert to all of them in under half a second and dropped no connections. It used 170 MB of memory and 1–2 % CPU at idle.

## Setup

| | |
|---|---|
| Machine | 2 vCPU, 8 GB RAM, Linux, SQLite on ext4 |
| Server | `qivo-server` release build (v0.6 + the fixes below) |
| Load generator | `tools/loadtest` (Rust). It ran **on the same 2 CPUs**, so the server had less than the whole machine. |
| Campus | 1 organization, 10 buildings (groups), 20 full-HD images, 3 playlists, 3 three-zone layouts, 4 schedules |
| Screens | 10,000 and 15,000 simulated players. Each one keeps a real WebSocket open, answers pings and, like the real player, re-downloads its manifest (15 KB) when told to refresh |

To reproduce: see `tools/loadtest/README.md`.

## Results: before and after the fixes (10,000 screens)

| What | Before | After |
|---|---|---|
| Time to connect 10,000 screens (1,000 per second requested) | 58.6 s | **10.3 s** (the requested rate) |
| Connection handshake p50 / p99 | 6.9 s / 14 s | **8 ms / 18 ms** |
| Server CPU used while connecting | 118 CPU-seconds | **3.7 CPU-seconds** |
| Memory with 10,000 screens connected | 1,900 MB (191 KB per screen) | **128 MB (10 KB per screen)** |
| Server CPU at idle with 10,000 screens connected | — | **0.6 %** |
| Spurious "refresh" messages while connecting | 27,435 | **0** |
| Errors (HTTP 500 "database pool timed out") | 284 | **0** |
| **Emergency alert: time to reach every screen** (p99 / max) | 7.5 s / 7.6 s | **0.34 s / 0.37 s** |
| Content change: time to reach every screen (p99) | 1.1 s | **0.25 s** |
| Time until every screen has downloaded the new manifest | 17 s | **5.2 s** (spread on purpose, see below) |
| Same, with every screen downloading at the same instant | — | **4.7 s, 0 errors** |
| Manifest downloads per second (sustained) | ~620 | **~2,400** |
| Proof-of-play: batches of 50 per second | 145 | **364** (18,000 plays/s) |
| Dashboard: list of 10,000 screens | 1.4 s | **0.49 s** |

## 15,000 screens (after the fixes)

| What | Result |
|---|---|
| Connected / errors / dropped | 15,000 / 0 / 0 |
| Memory | 169 MB (9.4 KB per screen) |
| Idle CPU | 1.6 % |
| Emergency alert to all 15,000 (p99 / max) | **0.46 s / 0.48 s** |
| Content change to all 15,000 (p99) | 0.36–0.49 s |
| Every screen has the new manifest | 7.9 s (spread) |
| Manifest throughput | ~2,300 per second, p99 22 ms at 32 in parallel |

15,000 is where this test machine ran out of file handles (20,000 per process), not where the server reached its limit. Its CPU and memory were mostly unused.

## What the test found, and what was fixed

1. **Every event woke every screen.** One shared channel carried every message. "Screen X came online", which only dashboards need, woke all 10,000 player connections, so connecting N screens cost N² wake-ups. The channel also overflowed, which sent screens about 27,000 spurious refreshes.
   → Each organization now has its own player channel and its own dashboard channel. Each message is serialized once and shared by all sockets (`src/hub.rs`).
2. **Emergency alerts were the slowest message.** An alert to "all screens" carried the list of 10,000 screen ids and was copied for every socket: 100 million string copies, 7.6 s.
   → The target list is now a set that all sockets share. Delivery takes 0.37 s.
3. **190 KB of buffers per connection.** The WebSocket library reserves a 128 KB read buffer per socket by default.
   → Buffers are now 4 KB, with a 64 KB message limit, which also protects against oversized messages. Memory went from 1.9 GB to 128 MB.
4. **A database write on every connect, disconnect and manifest download** exhausted the connection pool (284 errors).
   → "Last seen" times are kept in memory and saved in one transaction every 15 seconds, and at shutdown. SQLite now runs with `synchronous=NORMAL`, which is crash-safe in WAL mode.
5. **The thundering herd.** After an edit, every screen downloaded its manifest in the same millisecond, and each download ran about 40 queries.
   → Manifest content is cached per organization and layout set. It is computed once even when thousands of screens ask at once, and emptied the moment content changes. A test checks that an edit appears on the very next download.
   → The refresh message carries `spread_ms`, about 0.5 ms per connected screen and at most 10 s. Each player waits a random moment within it. Emergency alerts are **not** delayed: their content travels inside the push message itself. Only the background manifest re-download is spread.
6. Dashboard lists checked whether each screen was online with one lock per screen. They now take a single lock.

## Limits and next steps

- **A university doesn't need more than this.** A large campus has 300–2,000 screens. This machine handled 15,000 with room to spare.
- **For the SaaS with many customers**, the next limits are the SQLite single writer (proof-of-play p99 reaches about 0.9 s under sustained heavy writes) and running one server process. Both are already on the Phase 2 roadmap: Postgres, and an event bus shared between several server instances.
- The dashboard's screen list returns every screen in one response, which takes about 0.5 s at 10,000. Paging it is a small change, to make when a customer passes about 2,000 screens.
- Not tested here: real network latency and packet loss, TLS through Caddy (it adds CPU per connection; plan about 1 vCPU per 10–20k screens), and media file downloads, which are static and cacheable and should go through a CDN for SaaS.
