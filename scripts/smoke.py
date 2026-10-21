#!/usr/bin/env python3
"""Stdio smoke: drive the real tackle binary as an MCP client (BRIEF-U0).

    smoke.py <tackle-binary> <transcript.json> [wait_seconds]

Calls both tools before any editor is connected (expects the honest
`no editor connected` error), then polls tackle_editor_state until the
editor session answers or wait_seconds pass, then calls both tools for real.
Writes every request, response and server stderr line to the transcript.
Exit 0 only if both tools returned real data.
"""
import json
import queue
import subprocess
import sys
import threading
import time

INIT = {"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "tackle-smoke", "version": "0"}}}
CALL_DEADLINE_S = 30


def main(binary, transcript_path, wait_s):
    server = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True, bufsize=1)
    stdout, stderr_lines, exchanges = queue.Queue(), [], []
    threading.Thread(target=lambda: [stdout.put(l) for l in server.stdout], daemon=True).start()
    threading.Thread(target=lambda: [stderr_lines.append(
        f"{time.strftime('%H:%M:%S')} {l.rstrip()}") for l in server.stderr], daemon=True).start()
    next_id = [1]

    def request(message, label):
        server.stdin.write(json.dumps(message) + "\n")
        server.stdin.flush()
        if "id" not in message:
            return None
        deadline = time.time() + CALL_DEADLINE_S
        while time.time() < deadline:
            response = json.loads(stdout.get(timeout=deadline - time.time()))
            if response.get("id") == message["id"]:
                exchanges.append({"label": label, "at": time.strftime("%H:%M:%S"),
                                  "request": message, "response": response})
                return response
        raise TimeoutError(label)

    def tool(name, label):
        next_id[0] += 1
        result = request({"jsonrpc": "2.0", "id": next_id[0], "method": "tools/call",
                          "params": {"name": name, "arguments": {}}}, label)["result"]
        return result.get("isError", False), result["content"][0]["text"]

    request(INIT, "initialize")
    request({"jsonrpc": "2.0", "method": "notifications/initialized"}, "initialized")
    next_id[0] += 1
    request({"jsonrpc": "2.0", "id": next_id[0], "method": "tools/list"}, "tools/list")
    tool("tackle_editor_state", "editor_state before editor connects")
    tool("tackle_scene_hierarchy", "scene_hierarchy before editor connects")
    polls, connected, started = 0, False, time.time()
    while time.time() - started < wait_s:
        next_id[0] += 1
        polls += 1
        server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": next_id[0], "method": "tools/call",
                                       "params": {"name": "tackle_editor_state", "arguments": {}}}) + "\n")
        server.stdin.flush()
        response = json.loads(stdout.get(timeout=CALL_DEADLINE_S))
        if not response["result"].get("isError"):
            connected = True
            break
        time.sleep(3)
    state_error, state = tool("tackle_editor_state", "editor_state live")
    tree_error, tree = tool("tackle_scene_hierarchy", "scene_hierarchy live")
    time.sleep(0.5)
    server.stdin.close()
    server.wait(timeout=10)
    ok = connected and not state_error and not tree_error
    with open(transcript_path, "w") as handle:
        json.dump({"ok": ok, "polls_until_connected": polls,
                   "seconds_until_connected": round(time.time() - started, 1),
                   "exchanges": exchanges, "server_stderr": stderr_lines}, handle, indent=1)
    print(f"connected={connected} after {polls} polls; editor_state error={state_error}; "
          f"scene_hierarchy error={tree_error}; transcript {transcript_path}")
    return 0 if ok else 1


if __name__ == "__main__":
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) == 4 else 300))
