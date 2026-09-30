#!/usr/bin/env python3
"""Exercise a fresh Compose API and worker using the deterministic mock transport."""

import json
import os
from pathlib import Path
import time
import urllib.request

BASE = os.environ["CHATFLOW_SMOKE_BASE_URL"].rstrip("/")
STATE = Path(os.environ["CHATFLOW_SMOKE_API_KEY_FILE"]).parent
KEY = (STATE / "api-key").read_text().strip()
FLOW = json.loads(Path(__file__).resolve().parent.parent.joinpath("examples/basic-chat.json").read_text())


def request(method, path, body=None):
    headers = {"Authorization": f"Bearer {KEY}"}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=10) as response:
        return json.load(response)


def run_flow(flow, message):
    published = request(
        "POST", f"/v1/flows/{flow['flow_id']}/versions", {"name": "Compose smoke", "definition": flow}
    )
    run = request(
        "POST", "/v1/runs", {"flow_version_id": published["version_id"], "input": {"message": message}}
    )
    for _ in range(120):
        current = request("GET", f"/v1/runs/{run['run_id']}")
        if current["status"] in ("succeeded", "failed", "cancelled"):
            break
        time.sleep(1)
    else:
        raise SystemExit("Compose run did not finish within 120 seconds")
    if current["status"] != "succeeded":
        raise SystemExit(f"Compose run ended with {current['status']}: {current.get('error')}")
    return current


mock = run_flow(FLOW, "compose smoke")
if "compose smoke" not in mock.get("output", {}).get("reply", ""):
    raise SystemExit("mock run returned the wrong output")
report = {"ready": True, "worker_run": mock["status"], "run_id": mock["run_id"]}

if (STATE / "ragflow-dataset-id").is_file() and (STATE / "ragflow-api-key").stat().st_size:
    rag_flow = json.loads(Path(__file__).resolve().parent.parent.joinpath("examples/rag-chat.json").read_text())
    rag = run_flow(rag_flow, "What is the verification phrase?")
    if "cedar moon 417" not in rag.get("output", {}).get("reply", ""):
        raise SystemExit("RAGFlow run did not return the indexed verification phrase")
    report["ragflow_run"] = rag["status"]
    report["ragflow_run_id"] = rag["run_id"]

print(json.dumps(report))
