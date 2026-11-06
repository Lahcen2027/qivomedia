# qivo-load — stress test

Simulates thousands of screens against a running `qivo-server`. Results from the last run are in `STRESS-TEST.md` at the repository root.

```bash
# 1. a fresh server (signup open, so the seed script can create the owner)
mkdir -p /tmp/st/data
QIVO_DATA_DIR=/tmp/st/data QIVO_ALLOW_SIGNUP=1 QIVO_BIND=127.0.0.1:8080 RUST_LOG=warn \
  ./target/release/qivo-server &

# 2. a campus with 10,000 paired screens (needs: pip install requests pillow)
python3 tools/loadtest/seed.py http://127.0.0.1:8080 /tmp/st/data/qivo.db 10000 /tmp/st/keys.txt

# 3. the test (raise the open-file limit: one socket per screen)
ulimit -n 20000
cd tools/loadtest && cargo build --release
./target/release/qivo-load --base http://127.0.0.1:8080 --keys /tmp/st/keys.txt \
  --email owner@campus.test --password "stress test campus 2026" \
  --n 10000 --rate 1000 --server-pid $(pgrep -f "release/qivo-server")
```

| Option | Meaning |
|---|---|
| `--n` | number of screens (up to the number of keys) |
| `--rate` | new connections per second |
| `--only refresh,alert,manifest,pop,dashboard` | run only some scenarios |
| `--no-herd` | screens don't re-download their manifest on refresh |
| `--ignore-spread` | screens re-download at the same instant (worst case, like after a server restart) |
| `--herd-conc` | maximum downloads in flight from the load generator (default 1000) |
| `--bench-secs` | duration of each HTTP benchmark |
| `--server-pid` | reports the server's memory and CPU |

Each scenario prints one JSON line. The full report is printed last on a line starting with `REPORT`.
