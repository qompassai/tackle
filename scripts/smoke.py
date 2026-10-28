#!/usr/bin/env python3
"""Stdio smoke: drive the real tackle binary as an MCP client (U1 read core).

    smoke.py <tackle-binary> <transcript.json> [wait_seconds]

Modern era (B1): the session opens with `server/discover` and every
request carries its protocol version in `params._meta`.
Calls all six tools plus two resource reads before any editor is connected
(expects the honest `no editor connected` error), then polls
tackle_editor_state until the editor session answers or wait_seconds pass,
then calls every tool and resource for real, including one paginated
hierarchy page and one gameobject-properties lookup.
Writes every request, response and server stderr line to the transcript.
Exit 0 only if every live call returned real data.
"""
import json
import queue
import subprocess
import sys
import threading
import time

META = {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "tackle-smoke", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}}
DISCOVER = {"jsonrpc": "2.0", "id": 1, "method": "server/discover",
            "params": {"_meta": META}}
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

    def tool(name, label, arguments=None):
        next_id[0] += 1
        result = request({"jsonrpc": "2.0", "id": next_id[0], "method": "tools/call",
                          "params": {"name": name, "arguments": arguments or {},
                                     "_meta": META}}, label)["result"]
        return result.get("isError", False), result["content"][0]["text"]

    def resource(uri, label):
        next_id[0] += 1
        response = request({"jsonrpc": "2.0", "id": next_id[0], "method": "resources/read",
                            "params": {"uri": uri, "_meta": META}}, label)
        if "error" in response:
            return True, json.dumps(response["error"])
        contents = response["result"]["contents"]
        return False, contents[0]["text"] if contents else ""

    request(DISCOVER, "server/discover")
    next_id[0] += 1
    request({"jsonrpc": "2.0", "id": next_id[0], "method": "tools/list",
             "params": {"_meta": META}}, "tools/list")
    next_id[0] += 1
    request({"jsonrpc": "2.0", "id": next_id[0], "method": "resources/list",
             "params": {"_meta": META}}, "resources/list")
    tool("tackle_editor_state", "editor_state before editor connects")
    tool("tackle_scene_hierarchy", "scene_hierarchy before editor connects")
    tool("tackle_gameobject_properties", "gameobject_properties before editor connects",
         {"path": "Ground"})
    tool("tackle_console", "console before editor connects")
    tool("tackle_project_info", "project_info before editor connects")
    tool("tackle_selection", "selection before editor connects")
    resource("tackle://editor/state", "resource state before editor connects")
    polls, connected, started = 0, False, time.time()
    while time.time() - started < wait_s:
        next_id[0] += 1
        polls += 1
        server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": next_id[0], "method": "tools/call",
                                       "params": {"name": "tackle_editor_state", "arguments": {},
                                                  "_meta": META}}) + "\n")
        server.stdin.flush()
        response = json.loads(stdout.get(timeout=CALL_DEADLINE_S))
        if not response["result"].get("isError"):
            connected = True
            break
        time.sleep(3)
    live = {}
    live["editor_state"] = tool("tackle_editor_state", "editor_state live")
    live["scene_hierarchy"] = tool("tackle_scene_hierarchy", "scene_hierarchy live")
    live["scene_hierarchy_page"] = tool(
        "tackle_scene_hierarchy", "scene_hierarchy live page 2",
        {"offset": 1, "limit": 2})
    live["gameobject_properties"] = tool(
        "tackle_gameobject_properties", "gameobject_properties live",
        {"path": "Seraphine (factory)"})
    live["gameobject_missing"] = tool(
        "tackle_gameobject_properties", "gameobject_properties live missing",
        {"path": "No Such Object"})
    live["console"] = tool("tackle_console", "console live", {"max_entries": 10})
    live["project_info"] = tool("tackle_project_info", "project_info live")
    live["selection"] = tool("tackle_selection", "selection live")
    live["resource_state"] = resource("tackle://editor/state", "resource state live")
    live["resource_hierarchy"] = resource(
        "tackle://editor/scene-hierarchy", "resource scene-hierarchy live")
    time.sleep(0.5)
    server.stdin.close()
    server.wait(timeout=10)
    # The missing-object lookup must fail honestly; everything else must pass.
    errors = {name: error for name, (error, _text) in live.items() if error}
    ok = connected and set(errors) == {"gameobject_missing"}
    with open(transcript_path, "w") as handle:
        json.dump({"ok": ok, "polls_until_connected": polls,
                   "seconds_until_connected": round(time.time() - started, 1),
                   "live_errors": sorted(errors),
                   "exchanges": exchanges, "server_stderr": stderr_lines}, handle, indent=1)
    print(f"connected={connected} after {polls} polls; live errors={sorted(errors)}; "
          f"transcript {transcript_path}")
    return 0 if ok else 1


if __name__ == "__main__":
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) == 4 else 300))
