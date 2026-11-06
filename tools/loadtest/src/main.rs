//! qivo-load — stress test for qivo-server.
//!
//! Opens thousands of player sockets (like real screens), then measures:
//!   * connection ramp, server memory and CPU
//!   * fan-out latency of a content change ("refresh") to every screen
//!   * emergency-alert delivery latency to every screen
//!   * the "thundering herd": every screen re-downloading its manifest at once
//!   * sustained HTTP throughput: manifest, proof-of-play, dashboard lists
//!
//! Usage:
//!   qivo-load --base http://127.0.0.1:8080 --keys keys.txt --email a@b --password '...' \
//!             [--n 10000] [--rate 1000] [--herd-conc 1000] [--bench-secs 15] [--server-pid PID]

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tokio_tungstenite::tungstenite::Message;

struct Args {
    base: String,
    keys: Vec<String>,
    email: String,
    password: String,
    n: usize,
    rate: usize,
    herd: bool,
    herd_conc: usize,
    ignore_spread: bool,
    bench_secs: u64,
    server_pid: Option<u32>,
    only: String,
}

fn args() -> Args {
    let a: Vec<String> = std::env::args().collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let keys: Vec<String> = std::fs::read_to_string(get("--keys").expect("--keys file"))
        .expect("read keys")
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let n = get("--n").map(|v| v.parse().unwrap()).unwrap_or(keys.len()).min(keys.len());
    Args {
        base: get("--base").unwrap_or_else(|| "http://127.0.0.1:8080".into()),
        keys,
        email: get("--email").expect("--email"),
        password: get("--password").expect("--password"),
        n,
        rate: get("--rate").map(|v| v.parse().unwrap()).unwrap_or(1000),
        herd: !a.iter().any(|x| x == "--no-herd"),
        ignore_spread: a.iter().any(|x| x == "--ignore-spread"),
        herd_conc: get("--herd-conc").map(|v| v.parse().unwrap()).unwrap_or(1000),
        bench_secs: get("--bench-secs").map(|v| v.parse().unwrap()).unwrap_or(15),
        server_pid: get("--server-pid").map(|v| v.parse().unwrap()),
        only: get("--only").unwrap_or_else(|| "all".into()),
    }
}

/// Arrival times of each message kind, reset per scenario.
#[derive(Default)]
struct Arrivals {
    by_kind: Mutex<HashMap<String, Vec<Instant>>>,
}
impl Arrivals {
    fn push(&self, kind: &str) {
        let now = Instant::now();
        self.by_kind.lock().unwrap().entry(kind.to_owned()).or_default().push(now);
    }
    fn count(&self, kind: &str) -> usize {
        self.by_kind.lock().unwrap().get(kind).map_or(0, Vec::len)
    }
    fn take(&self) -> HashMap<String, Vec<Instant>> {
        std::mem::take(&mut *self.by_kind.lock().unwrap())
    }
}

#[derive(Clone)]
struct Ctx {
    base: String,
    http: reqwest::Client,
    arrivals: Arc<Arrivals>,
    herd: bool,
    herd_sem: Arc<Semaphore>,
    ignore_spread: bool,
    connected: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(Instant::now().elapsed().as_nanos());
    h.finish()
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

fn summary(ms: &mut Vec<f64>) -> Value {
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    json!({
        "count": ms.len(),
        "p50_ms": round(pct(ms, 0.50)), "p95_ms": round(pct(ms, 0.95)),
        "p99_ms": round(pct(ms, 0.99)), "max_ms": round(ms.last().copied().unwrap_or(f64::NAN)),
    })
}
fn round(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

struct ProcSample {
    rss_mb: f64,
    cpu_ticks: u64,
    threads: u64,
}
fn proc_sample(pid: Option<u32>) -> Option<ProcSample> {
    let pid = pid?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = |name: &str| -> u64 {
        status.lines().find(|l| l.starts_with(name)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
    };
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = after.split_whitespace().collect();
    let cpu_ticks = f[11].parse::<u64>().ok()? + f[12].parse::<u64>().ok()?;
    Some(ProcSample { rss_mb: field("VmRSS:") as f64 / 1024.0, cpu_ticks, threads: field("Threads:") })
}

async fn fetch_manifest(http: &reqwest::Client, base: &str, key: &str) -> anyhow::Result<usize> {
    let r = http.get(format!("{base}/api/player/manifest")).header("x-device-key", key).send().await?;
    let s = r.status();
    let b = r.bytes().await?;
    anyhow::ensure!(s.is_success(), "status {s}");
    Ok(b.len())
}

/// One simulated screen: holds the socket open, records every push it receives and,
/// like the real player, re-downloads its manifest when told to refresh.
async fn screen(ctx: Ctx, key: String, connect_ms: Arc<Mutex<Vec<f64>>>) {
    let ws_url = format!("{}/ws/player?key={}", ctx.base.replacen("http", "ws", 1), key);
    let t = Instant::now();
    let (mut ws, _) = match tokio_tungstenite::connect_async(&ws_url).await {
        Ok(x) => x,
        Err(_) => {
            ctx.arrivals.push("connect_error");
            return;
        }
    };
    connect_ms.lock().unwrap().push(t.elapsed().as_secs_f64() * 1000.0);
    ctx.connected.fetch_add(1, Ordering::Relaxed);
    while let Some(msg) = ws.next().await {
        match msg {
            Ok(Message::Text(t)) => {
                let v: Value = serde_json::from_str(&t).unwrap_or_default();
                let kind = v["type"].as_str().unwrap_or("?").to_owned();
                ctx.arrivals.push(&kind);
                if kind == "refresh" && ctx.herd {
                    let c = ctx.clone();
                    let k = key.clone();
                    // Same as the real player: wait a random moment within the server's spread.
                    let spread = v["spread_ms"].as_u64().unwrap_or(0).min(10_000);
                    let wait = if spread == 0 || ctx.ignore_spread { 0 } else { rand_u64() % spread };
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(wait)).await;
                        let _p = c.herd_sem.acquire().await;
                        match fetch_manifest(&c.http, &c.base, &k).await {
                            Ok(_) => c.arrivals.push("manifest_ok"),
                            Err(_) => c.arrivals.push("manifest_err"),
                        }
                    });
                }
            }
            Ok(Message::Ping(p)) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    ctx.connected.fetch_sub(1, Ordering::Relaxed);
    ctx.dropped.fetch_add(1, Ordering::Relaxed);
}

struct Admin {
    http: reqwest::Client,
    base: String,
    cookie: String,
}
impl Admin {
    async fn login(base: &str, email: &str, password: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::new();
        let r = http
            .post(format!("{base}/api/auth/login"))
            .header("origin", base)
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await?;
        anyhow::ensure!(r.status().is_success(), "login failed: {}", r.status());
        let cookie = r
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.contains("qivo_session="))
            .and_then(|c| c.split(';').next())
            .ok_or_else(|| anyhow::anyhow!("no session cookie"))?
            .to_owned();
        Ok(Self { http, base: base.into(), cookie })
    }
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<(u16, Value)> {
        let m = reqwest::Method::from_bytes(method.as_bytes())?;
        let mut rq = self.http.request(m, format!("{}{}", self.base, path)).header("cookie", &self.cookie).header("origin", &self.base);
        if let Some(b) = body {
            rq = rq.json(&b);
        }
        let r = rq.send().await?;
        let s = r.status().as_u16();
        let v = r.json::<Value>().await.unwrap_or(Value::Null);
        Ok((s, v))
    }
}

/// Fire an action and measure how long until `n` screens received `kind`
/// (and, with the herd on, until they all re-downloaded their manifest).
async fn fanout(ctx: &Ctx, n: usize, name: &str, kind: &str, expect_herd: bool, action: impl std::future::Future<Output = anyhow::Result<(u16, Value)>>) -> Value {
    ctx.arrivals.take();
    tokio::time::sleep(Duration::from_millis(300)).await;
    ctx.arrivals.take();
    let t0 = Instant::now();
    let (status, _) = action.await.unwrap_or((0, Value::Null));
    let api_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let deadline = t0 + Duration::from_secs(120);
    let herd = expect_herd && ctx.herd;
    loop {
        let got = ctx.arrivals.count(kind);
        let man = ctx.arrivals.count("manifest_ok") + ctx.arrivals.count("manifest_err");
        if (got >= n && (!herd || man >= n)) || Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let arr = ctx.arrivals.take();
    let lat = |k: &str| -> Vec<f64> { arr.get(k).map(|v| v.iter().map(|t| (*t - t0).as_secs_f64() * 1000.0).collect()).unwrap_or_default() };
    let mut delivered = lat(kind);
    let mut out = json!({
        "scenario": name, "api_status": status, "api_ms": round(api_ms), "screens": n,
        "delivered": summary(&mut delivered),
    });
    if herd {
        let mut m = lat("manifest_ok");
        out["manifest_refetch"] = summary(&mut m);
        out["manifest_errors"] = json!(arr.get("manifest_err").map_or(0, Vec::len));
    }
    let extra: HashMap<&String, usize> = arr.iter().filter(|(k, _)| k.as_str() != kind && !k.starts_with("manifest")).map(|(k, v)| (k, v.len())).collect();
    if !extra.is_empty() {
        out["other_messages"] = json!(extra);
    }
    println!("{}", serde_json::to_string(&out).unwrap());
    out
}

/// Hammer an endpoint with `conc` workers for `secs` seconds.
async fn bench<F, Fut>(name: &str, conc: usize, secs: u64, f: F) -> Value
where
    F: Fn(usize) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = bool> + Send,
{
    let f = Arc::new(f);
    let lat = Arc::new(Mutex::new(Vec::<f64>::new()));
    let errs = Arc::new(AtomicUsize::new(0));
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(secs);
    let mut hs = vec![];
    for w in 0..conc {
        let (f, lat, errs) = (f.clone(), lat.clone(), errs.clone());
        hs.push(tokio::spawn(async move {
            let mut i = w;
            let mut mine = vec![];
            while Instant::now() < deadline {
                let t = Instant::now();
                if f(i).await {
                    mine.push(t.elapsed().as_secs_f64() * 1000.0);
                } else {
                    errs.fetch_add(1, Ordering::Relaxed);
                }
                i += conc;
            }
            lat.lock().unwrap().extend(mine);
        }));
    }
    for h in hs {
        let _ = h.await;
    }
    let el = t0.elapsed().as_secs_f64();
    let mut l = std::mem::take(&mut *lat.lock().unwrap());
    let ok = l.len();
    let out = json!({
        "bench": name, "concurrency": conc, "seconds": round(el),
        "requests_per_s": round(ok as f64 / el), "errors": errs.load(Ordering::Relaxed),
        "latency": summary(&mut l),
    });
    println!("{}", serde_json::to_string(&out).unwrap());
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a = args();
    let admin = Arc::new(Admin::login(&a.base, &a.email, &a.password).await?);
    let http = reqwest::Client::builder().pool_max_idle_per_host(a.herd_conc).build()?;
    let ctx = Ctx {
        base: a.base.clone(),
        http: http.clone(),
        arrivals: Default::default(),
        herd: a.herd,
        herd_sem: Arc::new(Semaphore::new(a.herd_conc)),
        ignore_spread: a.ignore_spread,
        connected: Default::default(),
        dropped: Default::default(),
    };
    let mut report = json!({ "screens": a.n, "connect_rate_per_s": a.rate, "herd_concurrency": a.herd_conc });
    let tick = 100.0; // clock ticks per second on Linux
    let idle = proc_sample(a.server_pid);
    if let Some(s) = &idle {
        report["server_idle"] = json!({ "rss_mb": round(s.rss_mb), "threads": s.threads });
    }

    // ---- 1. connection ramp ----
    let connect_ms = Arc::new(Mutex::new(Vec::new()));
    let t0 = Instant::now();
    let cpu0 = idle.as_ref().map(|s| s.cpu_ticks).unwrap_or(0);
    let batch = (a.rate / 20).max(1);
    for (i, key) in a.keys.iter().take(a.n).enumerate() {
        tokio::spawn(screen(ctx.clone(), key.clone(), connect_ms.clone()));
        if (i + 1) % batch == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    while ctx.arrivals.count("hello") + ctx.arrivals.count("connect_error") < a.n && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let ramp = t0.elapsed().as_secs_f64();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut cm = std::mem::take(&mut *connect_ms.lock().unwrap());
    let arr = ctx.arrivals.take();
    let mut conn = json!({
        "connected": ctx.connected.load(Ordering::Relaxed),
        "hello_received": arr.get("hello").map_or(0, Vec::len),
        "errors": arr.get("connect_error").map_or(0, Vec::len),
        "ramp_seconds": round(ramp),
        "handshake": summary(&mut cm),
        "unexpected_refresh": arr.get("refresh").map_or(0, Vec::len),
    });
    if let (Some(s), Some(i)) = (proc_sample(a.server_pid), &idle) {
        conn["server_rss_mb"] = json!(round(s.rss_mb));
        conn["kb_per_socket"] = json!(round((s.rss_mb - i.rss_mb) * 1024.0 / a.n as f64));
        conn["server_cpu_seconds_during_ramp"] = json!(round((s.cpu_ticks - cpu0) as f64 / tick));
        conn["threads"] = json!(s.threads);
    }
    println!("{}", serde_json::to_string(&json!({ "connections": conn })).unwrap());
    report["connections"] = conn;

    // idle CPU with all sockets open
    if let Some(s0) = proc_sample(a.server_pid) {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let s1 = proc_sample(a.server_pid).unwrap();
        report["idle_cpu_percent_with_sockets"] = json!(round((s1.cpu_ticks - s0.cpu_ticks) as f64 / tick / 10.0 * 100.0));
    }

    // Find something to edit.
    let (_, pls) = admin.call("GET", "/api/playlists", None).await?;
    let pl = pls.as_array().and_then(|a| a.first()).cloned().expect("seed a playlist first");
    let pl_id = pl["id"].as_str().unwrap().to_owned();
    let n = ctx.connected.load(Ordering::Relaxed);
    let mut scen = vec![];
    let want = |s: &str| a.only == "all" || a.only.split(',').any(|x| x == s);

    // ---- 2. content change fan-out ----
    if want("refresh") {
        for round_no in 0..3 {
            let ad = admin.clone();
            let (id, body) = (pl_id.clone(), json!({ "name": format!("Campus loop {round_no}"), "items": pl["items"], "shuffle": false }));
            let s0 = proc_sample(a.server_pid);
            let mut r = fanout(&ctx, n, &format!("content_change_{round_no}"), "refresh", true, async move { ad.call("PUT", &format!("/api/playlists/{id}"), Some(body)).await }).await;
            if let (Some(s0), Some(s1)) = (s0, proc_sample(a.server_pid)) {
                r["server_cpu_seconds"] = json!(round((s1.cpu_ticks - s0.cpu_ticks) as f64 / tick));
                r["server_rss_mb_after"] = json!(round(s1.rss_mb));
            }
            scen.push(r);
        }
    }

    // ---- 3. emergency alert to every screen ----
    if want("alert") {
        for round_no in 0..2 {
            let ad = admin.clone();
            let s0 = proc_sample(a.server_pid);
            let mut r = fanout(&ctx, n, &format!("emergency_alert_{round_no}"), "alert", true, async move { ad.call("POST", "/api/alerts", Some(json!({ "kind": "fire", "target_type": "all" }))).await }).await;
            if let (Some(s0), Some(s1)) = (s0, proc_sample(a.server_pid)) {
                r["server_cpu_seconds"] = json!(round((s1.cpu_ticks - s0.cpu_ticks) as f64 / tick));
                r["server_rss_mb_after"] = json!(round(s1.rss_mb));
            }
            scen.push(r);
            let ad = admin.clone();
            scen.push(fanout(&ctx, n, &format!("all_clear_{round_no}"), "alert_cleared", true, async move { ad.call("POST", "/api/alerts/clear-all", Some(json!({}))).await }).await);
        }
    }
    report["fanout"] = json!(scen);

    // ---- 4. sustained HTTP load (sockets stay connected) ----
    let mut benches = vec![];
    let keys = Arc::new(a.keys[..a.n].to_vec());
    if want("manifest") {
        for conc in [32usize, 256] {
            let (h, b, k) = (http.clone(), a.base.clone(), keys.clone());
            benches.push(bench("manifest", conc, a.bench_secs, move |i| {
                let (h, b, k) = (h.clone(), b.clone(), k.clone());
                async move { fetch_manifest(&h, &b, &k[i % k.len()]).await.is_ok() }
            }).await);
        }
    }
    if want("pop") {
        let (h, b, k) = (http.clone(), a.base.clone(), keys.clone());
        let batch: Value = json!((0..50).map(|j| json!({ "layout_id": null, "media_id": null, "item_type": "text", "started_at": format!("2026-11-06T10:{:02}:00Z", j), "duration_ms": 10000 })).collect::<Vec<_>>());
        benches.push(bench("proof_of_play_batch50", 32, a.bench_secs, move |i| {
            let (h, b, k, body) = (h.clone(), b.clone(), k.clone(), batch.clone());
            async move {
                matches!(h.post(format!("{b}/api/player/pop")).header("x-device-key", &k[i % k.len()]).json(&body).send().await, Ok(r) if r.status().is_success())
            }
        }).await);
    }
    if want("dashboard") {
        for (name, path) in [("dashboard_list_displays", "/api/displays"), ("dashboard_reports_overview", "/api/reports/overview"), ("dashboard_groups", "/api/groups")] {
            let ad = admin.clone();
            benches.push(bench(name, 4, a.bench_secs.min(8), move |_| {
                let ad = ad.clone();
                async move { matches!(ad.call("GET", path, None).await, Ok((200, _))) }
            }).await);
        }
    }
    report["http"] = json!(benches);
    report["sockets_dropped_during_test"] = json!(ctx.dropped.load(Ordering::Relaxed));
    report["sockets_still_connected"] = json!(ctx.connected.load(Ordering::Relaxed));
    if let Some(s) = proc_sample(a.server_pid) {
        report["server_final_rss_mb"] = json!(round(s.rss_mb));
    }
    println!("REPORT {}", serde_json::to_string(&report).unwrap());
    Ok(())
}
