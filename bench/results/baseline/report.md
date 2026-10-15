# Load test: original (PHP) vs pelican-rs

Data scale 1 (1000 users, 5000 servers, 100000 activity entries); 32 connections, 20s per run; each panel limited to 2 CPUs / 1 GiB.

| scenario | original rps | pelican-rs rps | speed-up | original p50 / p99 ms | pelican-rs p50 / p99 ms | original CPU % / MiB | pelican-rs CPU % / MiB | check: status/size, 2xx under load (orig; rs) |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| client-servers | 8.1 | 42.8 | 5.3× | 4073.0 / 7710.4 | 764.0 / 836.7 | 198 / 251 | 163 / 48 | 200/91650B, 100%; 200/91648B, 100% |
| client-server | 49.8 | 1341.5 | 26.9× | 636.5 / 839.9 | 23.7 / 28.7 | 199 / 221 | 176 / 48 | 200/1877B, 100%; 200/1877B, 100% |
| client-activity | 61.0 | 2356.2 | 38.6× | 515.9 / 688.9 | 13.4 / 17.3 | 198 / 216 | 188 / 47 | 200/713B, 100%; 200/713B, 100% |
| app-servers | 29.9 | 67.2 | 2.2× | 1083.0 / 1639.7 | 477.7 / 510.3 | 198 / 233 | 183 / 47 | 200/46942B, 100%; 200/46940B, 100% |
| app-users | 34.0 | 337.9 | 9.9× | 948.0 / 1312.9 | 93.8 / 111.1 | 184 / 223 | 182 / 47 | 200/17008B, 100%; 200/17006B, 100% |
| app-server | 52.6 | 1378.8 | 26.2× | 607.2 / 773.5 | 23.0 / 27.9 | 198 / 216 | 178 / 47 | 200/3228B, 100%; 200/3228B, 100% |
| remote-servers | 8.8 | 64.0 | 7.3× | 3734.0 / 7176.2 | 502.0 / 556.0 | 199 / 258 | 195 / 50 | 200/123800B, 100%; 200/123768B, 100% |
| remote-server | 63.1 | 2085.1 | 33.0× | 504.9 / 649.5 | 15.2 / 18.8 | 198 / 220 | 196 / 50 | 200/2388B, 100%; 200/2388B, 100% |
| web-login | 56.6 | 1991.7 | 35.2× | 555.4 / 731.2 | 12.6 / 70.6 | 184 / 232 | 60 / 51 | 200/33723B, 100%; 200/3394B, 100% |

Notes on this run (2026-10-15, test server: Ryzen 9 5950X; DB, measured panel and load generator each on their
own pinned core(s), next to running game servers):

- Code: m9 at `d924278` (before batching related records in lists).
- `client-activity` measured an empty page: the seeding attached every activity entry to a server, and account
  activity lists entries about the user. Fixed in `crates/difftest/src/bench.rs` for later runs.
- Response sizes differ by 2 bytes per pagination link: the links contain the host name (`panel-orig` vs `panel-rs`).
- `web-login` pages are different markup by design (UI parity, not byte parity). pelican-rs stores sessions in the
  database (ADR-0013), so a cookie-less request costs a session INSERT; it is bound by the database, not CPU.
