#!/usr/bin/env bash
# Free disk space in the lima VM "pelican" (builds live in docker volumes there).
#   scripts/clean.sh            cargo clean in every pelican-rs target volume not in use,
#                               remove target volumes / difftest envs of removed worktrees,
#                               prune docker build cache and dangling images
#   scripts/clean.sh --all      also clean volumes that are in use (stop builds first!)
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ALL="${1:-}"
limactl shell pelican -- sh -c '
set -e
busy=$(docker ps --no-trunc --format "{{.Mounts}}" | tr "," "\n" | grep pelican-rs-target || true)
for v in $(docker volume ls --format "{{.Name}}" | grep "^pelican-rs-target"); do
  env=${v#pelican-rs-target-}
  if [ "$v" != "pelican-rs-target" ] && [ "$v" != "pelican-rs-target-main" ] && [ ! -d "'"$REPO"'/.worktrees/$env" ]; then
    docker volume rm "$v" >/dev/null && echo "removed volume $v (worktree gone)" && continue
  fi
  if echo "$busy" | grep -qx "$v" && [ "'"$ALL"'" != "--all" ]; then echo "skip $v (in use)"; continue; fi
  docker run --rm -v "$v":/target -e CARGO_TARGET_DIR=/target -v "'"$REPO"'":"'"$REPO"'" -w "'"$REPO"'" pelican-rs-dev cargo clean -q 2>/dev/null || true
  echo "cleaned $v"
done
for p in $(docker compose ls -a --format json 2>/dev/null | grep -o "pelican-difftest-[a-z0-9_-]*" | sort -u); do
  env=${p#pelican-difftest-}
  if [ "$env" != "main" ] && [ ! -d "'"$REPO"'/.worktrees/$env" ]; then
    (cd "'"$REPO"'/difftest" && docker compose -p "$p" down -v >/dev/null 2>&1) && echo "removed difftest env $env"
  fi
done
docker builder prune -af >/dev/null 2>&1 || true
docker image prune -f >/dev/null 2>&1 || true
sudo fstrim -a >/dev/null 2>&1 || true
df -h / | tail -1
'
