#!/usr/bin/env bash
# Run one glm-worker task from a template: run.sh <template.md> <log-name> KEY=VALUE...
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tpl="$1"; name="$2"; shift 2
task="$(cat "$tpl")"
for kv in "$@"; do k="${kv%%=*}"; v="${kv#*=}"; task="${task//\{\{$k\}\}/$v}"; done
export GLM_WORKDIR="$REPO"
export GLM_ALLOWED_TOOLS="Read,Edit,Write,Glob,Grep,Bash(scripts/cargo check:*),Bash(scripts/cargo build:*),Bash(scripts/cargo test:*),Bash(scripts/cargo clippy:*),Bash(cargo fmt:*),Bash(ls:*),Bash(cat:*),Bash(head:*),Bash(wc:*),Bash(diff:*),Bash(grep:*)"
mkdir -p "$REPO/.worker-out"
"$REPO/../glm-worker.sh" "$task" < /dev/null > "$REPO/.worker-out/$name.txt" 2>&1 || echo "EXIT $?" >> "$REPO/.worker-out/$name.txt"
