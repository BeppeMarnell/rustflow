#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: scripts/full-compose.sh {up|verify|down|reset} [TENANT_NAME]" >&2
  exit 2
fi
command_name="$1"
tenant_name="${2:-local-dev}"
state_dir="${CHATFLOW_STATE_DIR:-$repo_root/.local}"
mkdir -p "$state_dir"
state_dir="$(cd "$state_dir" && pwd)"
export CHATFLOW_STATE_DIR="$state_dir"
export RAGFLOW_PROVISION_STATE_DIR="$state_dir"
export CHATFLOW_RAGFLOW_API_KEY_FILE="$state_dir/ragflow-api-key"
export CHATFLOW_RAGFLOW_NETWORK="${CHATFLOW_RAGFLOW_NETWORK:-${RAGFLOW_COMPOSE_PROJECT:-chatflow-ragflow}-link}"
export RAGFLOW_BASE_URL="http://ragflow-cpu:9380"

case "$command_name" in
  up)
    scripts/ragflow-bootstrap.sh
    scripts/compose-environment.sh up "$tenant_name"
    scripts/compose-environment.sh verify
    ;;
  verify)
    scripts/compose-environment.sh verify
    ;;
  down)
    scripts/compose-environment.sh down
    scripts/local-ragflow.sh stop
    ;;
  reset)
    scripts/compose-environment.sh reset
    scripts/local-ragflow.sh down -v --remove-orphans
    docker network rm "$CHATFLOW_RAGFLOW_NETWORK" >/dev/null 2>&1 || true
    rm -f "$state_dir/ragflow-api-key" "$state_dir/ragflow-dataset-id" "$state_dir/ragflow-document-id"
    echo "Full local Compose environment removed; run scripts/full-compose.sh up to recreate it"
    ;;
  *)
    echo "unknown command: $command_name" >&2
    exit 2
    ;;
esac
