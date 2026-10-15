#!/usr/bin/env bash
# Load and speed test: the original panel vs pelican-rs on the same data (bench/README.md).
#   bench/run.sh [--scale N] [--duration 20s] [--connections 32] [--only <scenario substring>]
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCALE=1; DURATION=20s; CONN=32; ONLY=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --scale) SCALE="$2"; shift 2 ;;
    --duration) DURATION="$2"; shift 2 ;;
    --connections) CONN="$2"; shift 2 ;;
    --only) ONLY="$2"; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
case "$REPO" in
  */.worktrees/*) VOLUME="pelican-rs-target-${REPO##*/.worktrees/}" ;;
  *) VOLUME="pelican-rs-target-main" ;;
esac
PROJECT=pelican-bench
NET="${PROJECT}_default"
DATA="$REPO/bench/.data"; mkdir -p "$DATA"
OUT="$REPO/bench/results/$(date +%Y%m%d-%H%M%S)"; mkdir -p "$OUT"
vm() { limactl shell pelican -- "$@"; }
compose() { vm sh -c "cd '$REPO/bench' && PELICAN_TARGET_VOLUME='$VOLUME' BENCH_DATA='$DATA' docker compose -p $PROJECT $*"; }

# Fixture secrets (difftest/fixtures/base.yaml, infra.yaml).
CLIENT_KEY="pacc_user0000001UserClientSecret0000000000000001"
APP_KEY_TOKEN="papp_admin000001AdminAppSecret000000000000000001"
NODE_TOKEN="difftestTokenId0.NodeDaemonToken0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKL"

echo "== release build"
PELICAN_TARGET_VOLUME="$VOLUME" "$REPO/scripts/cargo" build -q --release -p pelican

echo "== database and the original panel (creates the schema)"
compose "up -d mariadb panel-orig >/dev/null 2>&1"
for _ in $(seq 1 120); do compose "logs panel-orig 2>&1" | grep -q "Caching Blade views" && break; sleep 2; done
sleep 5

echo "== seeding (scale $SCALE)"
PELICAN_NET="$NET" PELICAN_TARGET_VOLUME="$VOLUME" "$REPO/scripts/cargo" run -q -p difftest -- \
  --backend mariadb --seed base,infra,client-users,schedules --bench-scale "$SCALE" | tail -2

echo "== pelican-rs"
compose "up -d --force-recreate panel-rs >/dev/null 2>&1"
sleep 5

auth_header() {
  case "$1" in
    client) echo "Authorization: Bearer $CLIENT_KEY" ;;
    app) echo "Authorization: Bearer $APP_KEY_TOKEN" ;;
    node) echo "Authorization: Bearer $NODE_TOKEN" ;;
    *) echo "X-Bench: 1" ;;
  esac
}

# One oha run inside the stand's network: JSON summary to stdout.
load() { # target method path header
  vm docker run --rm --network "$NET" ghcr.io/hatoo/oha:latest --no-tui --output-format json \
    -z "$DURATION" -c "$CONN" -m "$2" -H "Accept: application/json" -H "$4" "http://$1$3"
}

# CPU/memory of the measured container while the load runs.
stats() { # container seconds
  local end=$((SECONDS + $2)) peak_mem=0 cpu_sum=0 n=0
  while (( SECONDS < end )); do
    line="$(vm docker stats --no-stream --format '{{.CPUPerc}} {{.MemUsage}}' "$1" 2>/dev/null || true)"
    cpu="${line%%%*}"; mem="$(echo "$line" | awk '{print $2}')"
    mib="$(python3 -c "import re,sys; m=re.match(r'([0-9.]+)([KMG]i?B)', sys.argv[1] or '0MiB'); v=float(m.group(1)) if m else 0; u=m.group(2) if m else 'MiB'; print(int(v*{'KiB':1/1024,'MiB':1,'GiB':1024,'kB':1/1000,'MB':1,'GB':1000}.get(u,1)))" "$mem")"
    (( mib > peak_mem )) && peak_mem=$mib
    cpu_sum=$(python3 -c "print(${cpu_sum}+${cpu:-0})"); n=$((n + 1))
  done
  python3 -c "print(f'{($cpu_sum)/max($n,1):.0f} {$peak_mem}')"
}

secs="${DURATION%s}"
echo "scenario,panel,status_ok,rps,p50_ms,p95_ms,p99_ms,cpu_pct,peak_mem_mib" > "$OUT/results.csv"
grep -v '^#' "$REPO/bench/scenarios.tsv" | while IFS=$'\t' read -r name method path auth note; do
  [[ -n "$ONLY" && "$name" != *"$ONLY"* ]] && continue
  for panel in panel-orig panel-rs; do
    hdr="$(auth_header "$auth")"
    code="$(vm docker run --rm --network "$NET" curlimages/curl:8.11.1 -s -o /dev/null -w '%{http_code}' -X "$method" -H "Accept: application/json" -H "$hdr" "http://$panel$path")"
    load "$panel" "$method" "$path" "$hdr" > /dev/null 2>&1 || true   # warm-up
    stats "${PROJECT}-${panel}-1" "$secs" > "$OUT/$name.$panel.stats" &
    load "$panel" "$method" "$path" "$hdr" > "$OUT/$name.$panel.json"
    wait
    python3 - "$OUT/$name.$panel.json" "$OUT/$name.$panel.stats" "$name" "$panel" "$code" >> "$OUT/results.csv" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
cpu, mem = open(sys.argv[2]).read().split()
s, p = r["summary"], r["latencyPercentiles"]
ok = sum(v for k, v in r.get("statusCodeDistribution", {}).items() if k.startswith("2"))
total = sum(r.get("statusCodeDistribution", {}).values()) or 1
print(f'{sys.argv[3]},{sys.argv[4]},{sys.argv[5]}:{ok * 100 // total}%,{s["requestsPerSec"]:.1f},'
      f'{p["p50"] * 1000:.1f},{p["p95"] * 1000:.1f},{p["p99"] * 1000:.1f},{cpu},{mem}')
PY
    tail -1 "$OUT/results.csv"
  done
done

python3 "$REPO/bench/report.py" "$OUT/results.csv" "$SCALE" "$DURATION" "$CONN" > "$OUT/report.md"
echo "== report: $OUT/report.md"
cat "$OUT/report.md"
