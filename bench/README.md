# Load and speed tests

`bench/run.sh` measures the original Pelican panel (PHP) and pelican-rs side by side on identical data
and resources, and writes a comparison report to `bench/results/<timestamp>/report.md`.

```bash
bench/run.sh                      # scale 1, 20 s per run, 32 connections
bench/run.sh --scale 4 --duration 30s --connections 64
bench/run.sh --only remote        # scenarios whose name contains "remote"
```

## Setup

- **Database:** MariaDB 11.4 (`compose.yml`, project `pelican-bench`, tmpfs, 512 MiB buffer pool, 2 CPUs).
- **Original:** image `pelican-orig:dev` (production PHP ini, OPcache on) with the official entrypoint
  (migrations, `filament:optimize`, `view:cache`), PHP-FPM + Caddy; cache and sessions on files as in the
  original's defaults, synchronous queue; no cron/queue workers while measuring.
- **pelican-rs:** the release build of the current checkout (`pelican serve`).
- **Fairness:** both panels get **2 CPUs and 1 GiB**, run the same configuration (`APP_ENV=production`, same
  `APP_KEY` and database), and are measured **one at a time** after a warm-up run of the same scenario.
- **Data:** the difftest fixtures plus `--bench-scale N` × (1000 users, 5000 servers with allocations and
  variables, 100000 activity entries) over 10 nodes (`crates/difftest/src/bench.rs`). The client key user owns
  50 servers; the fixture node hosts a tenth of all servers.
- **Load:** [oha](https://github.com/hatoo/oha) in a container on the stand's network; requests per second,
  latency percentiles and status codes from its JSON summary; CPU and peak memory of the measured container
  sampled with `docker stats` during the run.

## Scenarios

`scenarios.tsv` — Client API (server list, one server, account activity), Application API (server and user
lists, a server with includes), Remote API (Wings boot: a node's server configurations, one configuration)
and a server-rendered page. Each scenario first checks the HTTP status on both panels; the report shows it
next to the share of successful responses, so a panel answering errors fast does not look good.

Results depend on the host; compare runs made on the same machine. Optimizations are checked by re-running
the stand and the differential tests (`scripts/difftest --backend all`).

## On a shared Linux server

The scripts run against the local Docker when `limactl` is absent. On a machine that also runs other
workloads (e.g. Wings with game servers), pin everything of the stand to a few whole cores and lower its CPU
weight, so the rest of the machine is not disturbed and the measurements are not either:

```bash
PELICAN_CPUSET=12-15,28-31 \
BENCH_CPUSET_DB=12,28 BENCH_CPUSET_PANEL=13,29 BENCH_CPUSET_LOAD=14,15,30,31 \
  bench/run.sh
```

`PELICAN_CPUSET` applies to the release build and the seeding (`scripts/cargo`, also used by
`scripts/difftest`); the other three pin the database, the measured panel and the load generator (here each
gets its own physical core(s): CPU `n` and `n+16` are the two threads of one core on that machine, see
`lscpu -e`). The stand publishes no ports; everything is in the `pelican-bench` compose project and its
network, and `docker compose -p pelican-bench down -v` removes it.
