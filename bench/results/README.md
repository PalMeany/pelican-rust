# Optimization runs

Test server (Ryzen 9 5950X; database, measured panel and load generator each pinned to their own core(s), 2 CPUs / 1 GiB per panel), data scale 1, 32 connections, 20 s per run. Reports: `baseline/`, `batch/`, `lto/`.

| run | code | change |
|---|---|---|
| baseline | `d924278` | M9 before optimization |
| batch | `cb81bd5` | server lists load eggs, allocations, mounts, nodes and transfers per page instead of per server |
| lto | `a34a4ac` | + user list Root Admin lookup per page; release build with thin LTO, one codegen unit, mimalloc |

Requests per second (pelican-rs; the original for comparison, from the last run):

| scenario | original | baseline | batch | lto | vs original |
|---|---:|---:|---:|---:|---:|
| client-servers | 8.1 | 42.8 | 106.5 | 110.9 | 13.7× |
| client-server | 49.4 | 1341.5 | 1340.6 | 1373.2 | 27.8× |
| client-activity | 30.9 | 2356.2 | 54.3 | 54.0 | 1.7× |
| app-servers | 29.9 | 67.2 | 163.6 | 182.3 | 6.1× |
| app-users | 33.9 | 337.9 | 339.6 | 1516.6 | 44.7× |
| app-server | 52.4 | 1378.8 | 1372.3 | 1458.7 | 27.8× |
| remote-servers | 8.8 | 64.0 | 150.5 | 174.7 | 19.9× |
| remote-server | 62.2 | 2085.1 | 2075.0 | 2240.5 | 36.0× |
| web-login | 57.7 | 1991.7 | 1960.4 | 1953.8 | 33.9× |

- `client-activity` in the baseline was an empty page (seeding bug, fixed for later runs); both panels are bound by the
  same database query there (count and sort over the user's ~10k entries), pelican-rs at 17–22 % CPU.
- After batching, most scenarios are limited by the database (one pinned core), not by pelican-rs: its CPU use is
  below 200 %. LTO + mimalloc cut CPU per request by ~15–20 % (e.g. `client-server` 193 % → 160 % at slightly more
  requests) at the cost of a higher resident set (mimalloc keeps freed pages: ~50–75 MiB instead of ~31–43 MiB);
  the original uses 215–256 MiB.
- `web-login`: different markup by design; pelican-rs writes a session row per cookie-less request (ADR-0013).
