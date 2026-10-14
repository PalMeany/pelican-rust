#!/usr/bin/env bash
# Run one glm-worker task from a template: run.sh <template.md> <log-name> KEY=VALUE...
#   WORKDIR=<path>  run inside that directory (e.g. a git worktree); default: repo root
#   ENV=<name>      difftest environment the worker may use (DIFFTEST_ENV=<name>)
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tpl="$1"; name="$2"; shift 2
task="$(cat "$tpl")"
workdir="$REPO"; envname=""
for kv in "$@"; do
  k="${kv%%=*}"; v="${kv#*=}"
  [[ "$k" == "WORKDIR" ]] && workdir="$v"
  [[ "$k" == "ENV" ]] && envname="$v"
  task="${task//\{\{$k\}\}/$v}"
done
task="${task//\{\{WORKDIR\}\}/$workdir}"
export GLM_WORKDIR="$workdir"
tools="Read,Edit,Write,Glob,Grep,Bash(scripts/cargo check:*),Bash(scripts/cargo build:*),Bash(scripts/cargo test:*),Bash(scripts/cargo clippy:*),Bash(cargo fmt:*),Bash(ls:*),Bash(cat:*),Bash(head:*),Bash(tail:*),Bash(wc:*),Bash(diff:*),Bash(grep:*),Bash(git diff:*),Bash(git status:*)"
[[ -n "$envname" ]] && tools="$tools,Bash(DIFFTEST_ENV=$envname scripts/difftest:*)"
export GLM_ALLOWED_TOOLS="$tools"
mkdir -p "$REPO/.worker-out"
"$REPO/../glm-worker.sh" "$task" < /dev/null > "$REPO/.worker-out/$name.txt" 2>&1 || echo "EXIT $?" >> "$REPO/.worker-out/$name.txt"
