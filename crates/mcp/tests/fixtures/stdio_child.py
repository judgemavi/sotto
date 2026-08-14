import json
import os
import subprocess
import sys
import time


mode = sys.argv[1]
with open("audit.json", "w", encoding="utf-8") as audit_file:
    json.dump(
        {
            "argv": sys.argv,
            "cwd": os.getcwd(),
            "environment_keys": sorted(os.environ.keys()),
        },
        audit_file,
    )

descendant = subprocess.Popen(
    ["/bin/sleep", "600"], start_new_session=(mode == "escape")
)
with open("descendant.pid", "w", encoding="utf-8") as pid_file:
    pid_file.write(str(descendant.pid))

if mode == "oversized":
    sys.stdout.write("x" * 4096)
    sys.stdout.flush()
    time.sleep(600)

if mode == "malformed":
    sys.stdout.write("{not-json}\n")
    sys.stdout.flush()
    time.sleep(600)

if mode in ("wait", "escape"):
    time.sleep(600)

sys.stderr.write("server-secret-stderr-canary" * 4096)
sys.stderr.flush()

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    with open("methods.txt", "a", encoding="utf-8") as methods_file:
        methods_file.write(f"{method}\n")
    request_id = request.get("id")
    if request_id is None:
        continue
    if method == "initialize":
        result = {
            "protocolVersion": "2026-07-28",
            "capabilities": {"resources": {}},
            "serverInfo": {"name": "bounded-fixture", "version": "1"},
        }
    elif method == "resources/list":
        result = {
            "resources": [
                {
                    "uri": "docs://fixture/known",
                    "name": "known",
                    "mimeType": "text/plain",
                }
            ]
        }
    elif method == "resources/read" and mode == "input-required":
        result = {"resultType": "input_required", "requestState": "server-secret-state"}
    elif method == "resources/read":
        result = {
            "contents": [
                {
                    "uri": "docs://fixture/known",
                    "mimeType": "text/plain",
                    "text": "bounded fixture resource",
                }
            ]
        }
    else:
        response = {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32601, "message": "unsupported"},
        }
        sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
        sys.stdout.flush()
        continue
    response = {"jsonrpc": "2.0", "id": request_id, "result": result}
    sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
    sys.stdout.flush()
