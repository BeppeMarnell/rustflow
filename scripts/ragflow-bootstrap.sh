#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
state_dir="${RAGFLOW_PROVISION_STATE_DIR:-$repo_root/.local}"
mkdir -p "$state_dir"
state_dir="$(cd "$state_dir" && pwd)"
project="${RAGFLOW_COMPOSE_PROJECT:-chatflow-ragflow}"
dataset_name="${RAGFLOW_DATASET_NAME:-chatflow-local}"
umask 077

scripts/local-ragflow.sh up -d --wait
provision_output="$(mktemp "$state_dir/ragflow-provision.XXXXXX")"
trap 'rm -f "$provision_output"' EXIT

docker exec -i -e "RAGFLOW_DATASET_NAME=$dataset_name" "${project}-ragflow-cpu-1" \
  python - < scripts/ragflow-provision.py > "$provision_output"
python3 - "$provision_output" "$state_dir" <<'PY'
import json
from pathlib import Path
import sys

payload = json.loads(Path(sys.argv[1]).read_text())
if not payload["api_key"].startswith("ragflow-") or payload["retrieval_chunks"] < 1:
    raise SystemExit("RAGFlow provisioning output failed validation")
state = Path(sys.argv[2])
for name, value in (
    ("ragflow-api-key", payload["api_key"]),
    ("ragflow-dataset-id", payload["dataset_id"]),
    ("ragflow-document-id", payload["document_id"]),
):
    path = state / name
    path.write_text(value)
    path.chmod(0o600)
print(f"RAGFlow ready: dataset {payload['dataset_id']}, {payload['retrieval_chunks']} retrieved chunks")
PY
