"""Markdown comparison table from bench/run.sh results."""
import csv
import sys
from collections import defaultdict

path, scale, duration, conns = sys.argv[1:5]
rows = defaultdict(dict)
for r in csv.DictReader(open(path)):
    rows[r["scenario"]][r["panel"]] = r

print(f"# Load test: original (PHP) vs pelican-rs\n")
print(f"Data scale {scale} ({int(scale) * 1000} users, {int(scale) * 5000} servers, {int(scale) * 100000} activity entries); "
      f"{conns} connections, {duration} per run; each panel limited to 2 CPUs / 1 GiB.\n")
print("| scenario | original rps | pelican-rs rps | speed-up | original p50 / p99 ms | pelican-rs p50 / p99 ms | original CPU % / MiB | pelican-rs CPU % / MiB | status (orig / rs) |")
print("|---|---:|---:|---:|---:|---:|---:|---:|---|")
for name, panels in rows.items():
    o, r = panels.get("panel-orig"), panels.get("panel-rs")
    if not o or not r:
        continue
    speed = float(r["rps"]) / float(o["rps"]) if float(o["rps"]) else 0
    print(f'| {name} | {o["rps"]} | {r["rps"]} | {speed:.1f}× | {o["p50_ms"]} / {o["p99_ms"]} | {r["p50_ms"]} / {r["p99_ms"]} '
          f'| {o["cpu_pct"]} / {o["peak_mem_mib"]} | {r["cpu_pct"]} / {r["peak_mem_mib"]} | {o["status_ok"]} / {r["status_ok"]} |')
