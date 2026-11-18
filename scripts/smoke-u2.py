#!/usr/bin/env python3
"""Stdio smoke: drive the real tackle binary as an MCP client (U2 write core).

    smoke-u2.py <tackle-binary> <transcript.json> [wait_seconds]

Modern era: the session opens with `server/discover` and every request
carries its protocol version in `params._meta`. Polls tackle_editor_state
until the editor session answers, then runs the U2 write sequence against
the live editor: a no-key refusal, create + idempotent replay, a refused
create with a missing parent (must leave no orphan), modify + a refused
bad-tag modify (must leave no partial mutation), component add/remove,
prefab instantiate, a batch that must undo as ONE step (with redo),
a failing batch that must roll back whole, scene save, script create +
patch with compile read-back, and a delete/undo round trip. Everything
the smoke created is deleted again and the scene is saved pristine;
the final hierarchy must equal the initial one name-for-name.
Exit 0 only if every check passed. All requests, responses, per-check
verdicts and server stderr go to the transcript.
"""
import json
import queue
import subprocess
import sys
import threading
import time
from collections import Counter

META = {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "tackle-smoke-u2", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}}
CALL_DEADLINE_S = 180  # script writes wait out a full editor compile


def main(binary, transcript_path, wait_s):
    server = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True, bufsize=1)
    stdout, stderr_lines, exchanges, checks = queue.Queue(), [], [], []
    threading.Thread(target=lambda: [stdout.put(l) for l in server.stdout], daemon=True).start()
    threading.Thread(target=lambda: [stderr_lines.append(
        f"{time.strftime('%H:%M:%S')} {l.rstrip()}") for l in server.stderr], daemon=True).start()
    next_id = [1]

    def check(label, ok, detail=""):
        checks.append({"label": label, "ok": bool(ok), "detail": str(detail)[:400]})
        print(f"{'PASS' if ok else 'FAIL'} {label} {detail if not ok else ''}")

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

    def tool_json(name, label, arguments=None):
        error, text = tool(name, label, arguments)
        try:
            return error, json.loads(text)
        except (ValueError, TypeError):
            return error, None

    def hierarchy_names():
        error, payload = tool_json("tackle_scene_hierarchy", "hierarchy snapshot",
                                   {"offset": 0, "limit": 500})
        if error or payload is None:
            return None
        return [node["name"] for node in payload["nodes"]]

    def props(path):
        return tool_json("tackle_gameobject_properties", f"properties {path}",
                         {"path": path})

    request({"jsonrpc": "2.0", "id": 1, "method": "server/discover",
             "params": {"_meta": META}}, "server/discover")
    next_id[0] += 1
    listed = request({"jsonrpc": "2.0", "id": next_id[0], "method": "tools/list",
                      "params": {"_meta": META}}, "tools/list")
    tool_names = [t["name"] for t in listed["result"]["tools"]]
    check("tools/list serves 18 tools", len(tool_names) == 18, tool_names)

    polls, connected, started = 0, False, time.time()
    while time.time() - started < wait_s:
        polls += 1
        error, _text = tool("tackle_editor_state", "editor_state poll")
        if not error:
            connected = True
            break
        time.sleep(3)
    check("editor connected", connected, f"after {polls} polls")
    if not connected:
        return finish(server, transcript_path, checks, exchanges, stderr_lines, polls, started)

    initial_names = hierarchy_names()
    check("initial hierarchy read", initial_names is not None)
    initial = Counter(initial_names or [])

    # The decide() gate: a write with no idempotency key is refused
    # before the editor is touched.
    error, text = tool("tackle_gameobject_create", "create without key",
                       {"name": "TackleSmokeRoot"})
    check("write without idempotency key refused", error, text)
    check("refusal left the scene untouched",
          Counter(hierarchy_names() or []) == initial)

    error, _payload = props("Ground")
    check("read regression: properties Ground", not error)

    # Create + idempotent replay: the same key and arguments replay
    # the original result and apply exactly once.
    create_args = {"name": "TackleSmokeRoot", "idempotency_key": "smoke-create-root"}
    error1, text1 = tool("tackle_gameobject_create", "create root", create_args)
    error2, text2 = tool("tackle_gameobject_create", "create root replay", create_args)
    check("create root", not error1, text1)
    check("idempotent replay returns the original result",
          not error2 and text2 == text1, text2)
    names = hierarchy_names() or []
    check("replay applied exactly once", names.count("TackleSmokeRoot") == 1, names)

    error, _p = tool_json("tackle_gameobject_create", "create child",
                          {"name": "TackleSmokeChild", "parent_path": "TackleSmokeRoot",
                           "idempotency_key": "smoke-create-child"})
    check("create child under parent", not error)
    error, _p = props("TackleSmokeRoot/TackleSmokeChild")
    check("child readable by path", not error)

    # A missing parent must refuse BEFORE anything exists (no orphan),
    # and the failure must not kill the session.
    error, text = tool("tackle_gameobject_create", "create with missing parent",
                       {"name": "TackleSmokeOrphan", "parent_path": "No Such Parent",
                        "idempotency_key": "smoke-create-orphan"})
    check("create with missing parent refused", error, text)
    check("no orphan left behind",
          "TackleSmokeOrphan" not in (hierarchy_names() or []))
    error, _p = props("Ground")
    check("session survives the refused create", not error)

    # Modify: rename-free field set incl. the array-form position.
    error, _p = tool_json("tackle_gameobject_modify", "modify root",
                          {"path": "TackleSmokeRoot", "active": False,
                           "position": [1.5, 2.5, 3.5],
                           "idempotency_key": "smoke-modify-root"})
    check("modify root", not error)
    error, payload = props("TackleSmokeRoot")
    pos = (payload or {}).get("localPosition", {})
    check("modify applied (active + position)",
          not error and payload["activeSelf"] is False
          and (pos.get("x"), pos.get("y"), pos.get("z")) == (1.5, 2.5, 3.5),
          payload)

    # A bad tag must refuse before any field is applied.
    error, text = tool("tackle_gameobject_modify", "modify with unknown tag",
                       {"path": "TackleSmokeRoot", "tag": "NoSuchTag",
                        "active": True, "idempotency_key": "smoke-modify-badtag"})
    check("modify with unknown tag refused", error, text)
    error, payload = props("TackleSmokeRoot")
    check("refused modify applied nothing",
          not error and payload["tag"] == "Untagged" and payload["activeSelf"] is False,
          payload)

    error, payload = tool_json("tackle_component_add", "component add",
                               {"path": "TackleSmokeRoot",
                                "component_type": "UnityEngine.Rigidbody",
                                "idempotency_key": "smoke-comp-add"})
    check("component add", not error
          and "UnityEngine.Rigidbody" in (payload or {}).get("componentTypes", []),
          payload)
    error, payload = tool_json("tackle_component_remove", "component remove",
                               {"path": "TackleSmokeRoot",
                                "component_type": "UnityEngine.Rigidbody",
                                "idempotency_key": "smoke-comp-remove"})
    check("component remove", not error
          and "UnityEngine.Rigidbody" not in (payload or {}).get("componentTypes", []),
          payload)

    # A fresh editor may still be running its first asset import
    # when the bridge starts serving; "prefab not found" is its
    # honest answer until the import lands. Failed calls are never
    # idempotency-recorded, so the same key may retry — bounded.
    prefab_deadline = time.time() + 90
    while True:
        error, text = tool("tackle_prefab_instantiate", "prefab instantiate",
                           {"prefab_path": "Assets/Prefabs/TackleSmokePrefab.prefab",
                            "idempotency_key": "smoke-prefab"})
        if not error or "prefab not found" not in text or time.time() >= prefab_deadline:
            break
        time.sleep(5)
    check("prefab instantiate", not error, text)
    error, _p = props("TackleSmokePrefab")
    check("prefab instance readable", not error)

    # A successful batch collapses to ONE undo step.
    error, payload = tool_json("tackle_batch_execute", "batch of two creates",
                               {"operations": [
                                   {"name": "tackle_gameobject_create",
                                    "arguments": {"name": "TackleBatchOne"}},
                                   {"name": "tackle_gameobject_create",
                                    "arguments": {"name": "TackleBatchTwo"}}],
                                "idempotency_key": "smoke-batch"})
    check("batch applied 2 ops", not error and (payload or {}).get("applied") == 2,
          payload)
    error, result = tool_json("tackle_undo", "undo the batch",
                              {"idempotency_key": "smoke-undo-batch"})
    check("undo reports performed", not error and (result or {}).get("performed") is True,
          result)
    gone = all(props(p)[0] for p in ("TackleBatchOne", "TackleBatchTwo"))
    check("one undo removed BOTH batch members", gone)
    error, _p = props("TackleSmokePrefab")
    check("batch undo left earlier writes alone", not error)
    error, result = tool_json("tackle_redo", "redo the batch",
                              {"idempotency_key": "smoke-redo-batch"})
    check("redo reports performed", not error and (result or {}).get("performed") is True,
          result)
    back = all(not props(p)[0] for p in ("TackleBatchOne", "TackleBatchTwo"))
    check("redo restored both batch members", back)
    error, _r = tool_json("tackle_undo", "undo the batch again",
                          {"idempotency_key": "smoke-undo-batch-2"})
    gone = all(props(p)[0] for p in ("TackleBatchOne", "TackleBatchTwo"))
    check("second undo removed both again", not error and gone)

    # A failing batch rolls back whole: neither member may exist.
    error, text = tool("tackle_batch_execute", "batch with a doomed member",
                       {"operations": [
                           {"name": "tackle_gameobject_create",
                            "arguments": {"name": "TackleBatchThree"}},
                           {"name": "tackle_gameobject_create",
                            "arguments": {"name": "TackleBatchFour",
                                          "parent_path": "No Such Parent"}}],
                        "idempotency_key": "smoke-batch-fail"})
    check("failing batch refused", error, text)
    check("failing batch says it rolled back", "rolled back" in text, text)
    absent = all(props(p)[0] for p in ("TackleBatchThree", "TackleBatchFour"))
    check("rolled-back batch left neither member", absent)

    error, payload = tool_json("tackle_scene_save", "scene save",
                               {"idempotency_key": "smoke-save"})
    check("scene save", not error and (payload or {}).get("saved") is True, payload)

    # Script create + patch with compile read-back. The compile drops
    # and re-makes the editor session (domain reload); poll it back.
    script = {"path": "Scripts/TackleSmoke.cs",
              "contents": "public class TackleSmoke\n{\n    public int Value = 1;\n}\n"}
    error, payload = tool_json("tackle_script_create", "script create",
                               {**script, "idempotency_key": "smoke-script-create"})
    check("script create settled with no diagnostics",
          not error and (payload or {}).get("created") is True
          and payload.get("compileSettled") is True
          and payload.get("diagnostics") == []
          and payload.get("compiled") is True, payload)
    error, payload = tool_json("tackle_script_patch", "script patch",
                               {"path": script["path"], "old_text": "Value = 1",
                                "new_text": "Value = 2",
                                "idempotency_key": "smoke-script-patch"})
    check("script patch settled with no diagnostics",
          not error and (payload or {}).get("patched") is True
          and payload.get("compileSettled") is True
          and payload.get("diagnostics") == []
          and payload.get("compiled") is True, payload)
    error, _p = props("Ground")
    check("session healthy after script compile", not error)

    # Delete is undoable.
    error, _p = tool_json("tackle_gameobject_delete", "delete child",
                          {"path": "TackleSmokeRoot/TackleSmokeChild",
                           "idempotency_key": "smoke-delete-child"})
    check("delete child", not error)
    error, _p = props("TackleSmokeRoot/TackleSmokeChild")
    check("deleted child is gone", error)
    error, result = tool_json("tackle_undo", "undo the delete",
                              {"idempotency_key": "smoke-undo-delete"})
    check("undo reports performed", not error and (result or {}).get("performed") is True,
          result)
    error, _p = props("TackleSmokeRoot/TackleSmokeChild")
    check("undo restored the deleted child", not error)

    # Cleanup: delete everything the smoke created, verify the scene
    # is name-for-name the one we found, and save it pristine.
    for label, path, key in (
            ("cleanup delete child", "TackleSmokeRoot/TackleSmokeChild", "smoke-clean-1"),
            ("cleanup delete root", "TackleSmokeRoot", "smoke-clean-2"),
            ("cleanup delete prefab instance", "TackleSmokePrefab", "smoke-clean-3")):
        error, text = tool("tackle_gameobject_delete", label,
                           {"path": path, "idempotency_key": key})
        check(label, not error, text)
    final_names = hierarchy_names()
    check("scene restored to the initial hierarchy",
          Counter(final_names or []) == initial,
          {"initial": sorted(initial), "final": sorted(Counter(final_names or []))})
    error, payload = tool_json("tackle_scene_save", "final scene save",
                               {"idempotency_key": "smoke-save-final"})
    check("final scene save", not error and (payload or {}).get("saved") is True, payload)

    return finish(server, transcript_path, checks, exchanges, stderr_lines, polls, started)


def finish(server, transcript_path, checks, exchanges, stderr_lines, polls, started):
    time.sleep(0.5)
    try:
        server.stdin.close()
        server.wait(timeout=10)
    except Exception:
        server.kill()
    failed = [c["label"] for c in checks if not c["ok"]]
    ok = bool(checks) and not failed
    with open(transcript_path, "w") as handle:
        json.dump({"ok": ok, "polls_until_connected": polls,
                   "seconds_until_connected": round(time.time() - started, 1),
                   "checks_failed": failed, "checks": checks,
                   "exchanges": exchanges, "server_stderr": stderr_lines},
                  handle, indent=1)
    print(f"checks {len(checks) - len(failed)}/{len(checks)} passed; "
          f"failed={failed}; transcript {transcript_path}")
    return 0 if ok else 1


if __name__ == "__main__":
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) == 4 else 300))
