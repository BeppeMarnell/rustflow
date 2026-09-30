"""Run two turns in one conversation with the deterministic mock flow."""

import json
import time
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent.parent
BASE = "http://127.0.0.1:3000"
KEY = (ROOT / ".local/api-key").read_text().strip()
FLOW = json.loads((ROOT / "examples/basic-chat.json").read_text())


def request(method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    headers = {"Authorization": f"Bearer {KEY}"}
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=10) as response:
        return json.load(response)


version = request(
    "POST",
    f"/v1/flows/{FLOW['flow_id']}/versions",
    {"name": "Example conversation", "definition": FLOW},
)
conversation_id = None

for message in ("Hello!", "How are you?"):
    body = {"flow_version_id": version["version_id"], "input": {"message": message}}
    if conversation_id is not None:
        body["conversation_id"] = conversation_id
    run = request("POST", "/v1/runs", body)
    conversation_id = run["conversation_id"]

    for _ in range(120):
        result = request("GET", f"/v1/runs/{run['run_id']}")
        if result["status"] in ("succeeded", "failed", "cancelled"):
            break
        time.sleep(1)
    else:
        raise TimeoutError(f"Run {run['run_id']} did not finish within 120 seconds")

    if result["status"] != "succeeded":
        raise RuntimeError(f"Run ended with {result['status']}: {result.get('error')}")
    print(f"You: {message}\nAssistant: {result['output']['reply']}\n")

messages = request("GET", f"/v1/conversations/{conversation_id}/messages")
print(json.dumps(messages, indent=2))
