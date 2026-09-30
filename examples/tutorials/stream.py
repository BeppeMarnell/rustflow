"""Stream token events from one run of the deterministic mock flow."""

import json
import sys
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent.parent
BASE = "http://127.0.0.1:3000"
KEY = (ROOT / ".local/api-key").read_text().strip()
FLOW = json.loads((ROOT / "examples/basic-chat.json").read_text())


def post(path, body):
    req = urllib.request.Request(
        BASE + path,
        data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as response:
        return json.load(response)


version = post(
    f"/v1/flows/{FLOW['flow_id']}/versions",
    {"name": "Streaming example", "definition": FLOW},
)
run = post(
    "/v1/runs",
    {"flow_version_id": version["version_id"], "input": {"message": "Hello!"}},
)

req = urllib.request.Request(
    f"{BASE}/v1/runs/{run['run_id']}/events",
    headers={"Authorization": f"Bearer {KEY}", "Accept": "text/event-stream"},
)

with urllib.request.urlopen(req, timeout=130) as response:
    event_type = None
    for raw_line in response:
        line = raw_line.decode("utf-8").rstrip("\r\n")
        if line.startswith("event:"):
            event_type = line[6:].strip()
        elif line.startswith("data:"):
            if event_type == "error":
                raise RuntimeError(line[5:].strip())
            event = json.loads(line[5:].strip())
            if event_type == "token":
                print(event["metadata"]["token"], end="", flush=True)
            elif event_type == "run.failed":
                print(f"\nRun failed: {event}", file=sys.stderr)
        elif not line:
            event_type = None

print()
