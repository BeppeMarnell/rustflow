#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

usage() {
  echo "usage: scripts/compose-environment.sh {up|verify|down|reset} [TENANT_NAME]" >&2
  exit 2
}
[[ $# -ge 1 && $# -le 2 ]] || usage
command_name="$1"
tenant_name="${2:-local-dev}"

state_dir="${CHATFLOW_STATE_DIR:-$repo_root/.local}"
mkdir -p "$state_dir"
state_dir="$(cd "$state_dir" && pwd)"
umask 077
export CHATFLOW_RAGFLOW_API_KEY_FILE="${CHATFLOW_RAGFLOW_API_KEY_FILE:-$state_dir/ragflow-api-key}"
export CHATFLOW_OPS_API_KEY_FILE="${CHATFLOW_OPS_API_KEY_FILE:-$state_dir/ops-api-key}"
export CHATFLOW_RAGFLOW_NETWORK="${CHATFLOW_RAGFLOW_NETWORK:-${RAGFLOW_COMPOSE_PROJECT:-chatflow-ragflow}-link}"
compose=(docker compose)
if [[ -n "${CHATFLOW_COMPOSE_PROJECT:-}" ]]; then
  compose+=(-p "$CHATFLOW_COMPOSE_PROJECT")
fi
if [[ -n "${CHATFLOW_COMPOSE_ENV_FILE:-}" ]]; then
  compose+=(--env-file "$CHATFLOW_COMPOSE_ENV_FILE")
fi
compose+=(--profile app)
api_port="${CHATFLOW_API_PORT:-3000}"
base_url="http://127.0.0.1:$api_port"

write_key() {
  local label="$1" path="$2" data="$3"
  [[ "$data" == cf_* ]] || { echo "Invalid $label from bootstrap" >&2; exit 1; }
  printf '%s' "$data" > "$path"
  chmod 600 "$path"
}

wait_ready() {
  local attempt
  for attempt in {1..60}; do
    if curl --silent --fail "$base_url/health/ready" >/dev/null; then
      return
    fi
    sleep 2
  done
  echo "API readiness timed out at $base_url" >&2
  return 1
}

case "$command_name" in
  up)
    docker network inspect "$CHATFLOW_RAGFLOW_NETWORK" >/dev/null 2>&1 || \
      docker network create "$CHATFLOW_RAGFLOW_NETWORK" >/dev/null
    # Compose needs a mounted file even when RAG retrieval has not yet been configured.
    # An empty file permits mock flows; configure a real RAGFlow key before using RAG nodes.
    if [[ ! -f "$CHATFLOW_RAGFLOW_API_KEY_FILE" ]]; then
      : > "$CHATFLOW_RAGFLOW_API_KEY_FILE"
      : > "$state_dir/ragflow-key-placeholder"
    fi
    chmod 600 "$CHATFLOW_RAGFLOW_API_KEY_FILE"
    [[ -f "$CHATFLOW_OPS_API_KEY_FILE" ]] || : > "$CHATFLOW_OPS_API_KEY_FILE"
    "${compose[@]}" up -d --wait postgres vault
    "${compose[@]}" build chatflow-api chatflow-worker
    bootstrap_output="$("${compose[@]}" run --rm --no-deps chatflow-api bootstrap "$tenant_name")"
    tenant_id="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^tenant_id: //p')"
    api_key="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^api_key: //p')"
    rag_config_key="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^rag_config_key: //p')"
    ops_api_key="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^ops_api_key: //p')"
    [[ -n "$tenant_id" ]] || { echo "Bootstrap did not return a tenant ID" >&2; exit 1; }
    write_key api_key "$state_dir/api-key" "$api_key"
    write_key rag_config_key "$state_dir/rag-config-key" "$rag_config_key"
    write_key ops_api_key "$CHATFLOW_OPS_API_KEY_FILE" "$ops_api_key"
    printf '%s\n' "$tenant_id" > "$state_dir/tenant-id"
    "${compose[@]}" up -d chatflow-api chatflow-worker prometheus grafana
    wait_ready
    if [[ -s "$CHATFLOW_RAGFLOW_API_KEY_FILE" && -s "$state_dir/ragflow-dataset-id" ]]; then
      CHATFLOW_API_URL="$base_url" CHATFLOW_STATE_DIR="$state_dir" \
        python3 scripts/assign-ragflow-dataset.py
    fi
    echo "Compose environment ready at $base_url (tenant $tenant_id)"
    ;;
  verify)
    wait_ready
    CHATFLOW_SMOKE_BASE_URL="$base_url" CHATFLOW_SMOKE_API_KEY_FILE="$state_dir/api-key" \
      python3 scripts/verify-compose.py
    ;;
  down)
    "${compose[@]}" down
    ;;
  reset)
    "${compose[@]}" down -v --remove-orphans
    rm -f "$state_dir/api-key" "$state_dir/rag-config-key" "$state_dir/ops-api-key" \
      "$state_dir/tenant-id" "$state_dir/provider-credential-id"
    if [[ -f "$state_dir/ragflow-key-placeholder" && ! -s "$CHATFLOW_RAGFLOW_API_KEY_FILE" ]]; then
      rm -f "$CHATFLOW_RAGFLOW_API_KEY_FILE"
    fi
    rm -f "$state_dir/ragflow-key-placeholder"
    echo "Compose containers, volumes, and generated application keys removed"
    ;;
  *) usage ;;
esac
