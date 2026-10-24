# tackle U1 — read core REPORT (2026-10-24, primo)

**Verdict: U1 read core complete, gated live.** Six read-only MCP tools and
five read-only resources proxy to a live Unity 6000.6.6f1 Editor on the
throwaway spike copy. Local commits only; no remote, no push (none
authorized for tackle).

## 1. Gates (all run on primo)

| Gate | Result |
|---|---|
| `cargo fmt --check` | clean (exit 0, no output) |
| `cargo clippy --all-targets -- -D warnings` | clean (0 errors) |
| `cargo test` | **33 passed, 0 failed** (bridge 17, capability 8, stdio 8). Split: 14 validation (`ok_`) / 19 adversarial (`adv_`). U0 was 28 (11/17); U1 adds 2 bridge args tests (validation) and 3 stdio tests (resources list validation; invalid-args + resource-no-editor adversarial). |
| `nix build` | success → `/nix/store/v12yp8ckkfkcs4m0v59w0fwvmy0qjhgq-tackle-0.2.0` (a first nix build of the same tree produced `gn226yjb…-tackle-0.2.0`; the rebuild followed a tool-description wording change only) |
| C# compile | `Tackle.Editor.dll` built; Editor log has **0 `error CS`** lines in the Tackle package (U0's pre-existing CS0618 warnings in the project's own `TestAnim` scripts are untouched) |
| Session connected | Editor log: `[Tackle] session connected to 127.0.0.1:9770 via System.Net.WebSockets.ClientWebSocket` |
| Smoke (`scripts/smoke.py` vs nix binary) | **ok, exit 0**: connected after 2 polls (3.5 s); every live tool and resource call returned real data except the designed missing-object case (see §2) |

## 2. Live round-trip (smoke transcript: `~/tackle-spike/evidence/smoke-transcript-u1.json`)

Before the editor connected, all six tools returned the honest
`no editor connected` error and the resource read returned JSON-RPC
`-32603` with the same message; the unknown resource URI returned
`-32002`.

Live, against the spike project's `TestAnim` scene:

- `tackle_editor_state`: Unity `6000.6.6f1`, project `project` at
  `/home/phaedrus/tackle-spike/project`, product `strobe-trackb`,
  scene `TestAnim` (`Assets/Scenes/TestAnim.unity`), not playing, not
  compiling, `onMainThread: true`.
- `tackle_scene_hierarchy` (no args): 4 roots, 11 nodes, depth cap 4,
  count cap 500, `truncatedByDepth: true` (matches U0 exactly, plus new
  `totalCount`/`offset`/`limit`/`pageTruncated` fields).
- `tackle_scene_hierarchy` (page: offset 1, limit 2): nodes
  `Key Light`, `Main Camera`; `totalCount: 11`, `pageTruncated: true`.
- `tackle_gameobject_properties` (`Seraphine (factory)`): active,
  layer `Default`, components `UnityEngine.Transform`,
  `UnityEngine.Animator`, `StageDriver`; local position `(0,0,0)`,
  scale `(1,1,1)`, 3 children.
- `tackle_gameobject_properties` (`No Such Object`): honest editor
  error `InvalidOperationException: gameobject not found` — and the
  session survived (later calls succeeded). This is the smoke's only
  expected live error.
- `tackle_console` (max 10): real entries via `UnityEditor.LogEntries`
  reflection, newest first — e.g. the `[Tackle] session connected` line
  with file `TackleBridge.cs`, line 90, raw `mode: 8406016`, and the
  Input Manager deprecation notice with `mode: 512`. Modes are raw
  Unity-internal values (see §4).
- `tackle_project_info`: product `strobe-trackb`, company
  `DefaultCompany`, bundle `1.0`, build target `StandaloneLinux64`,
  active scene `TestAnim`, `buildSceneCount: 0`.
- `tackle_selection`: empty selection, honestly reported
  (`selectedCount: 0`, `activeObjectName: null`, `otherSelectedCount: 0`).
- Resources: `tackle://editor/state` and
  `tackle://editor/scene-hierarchy` read back the same data as their
  tools; `resources/list` returns exactly the five URIs below.

## 3. What was built

- **Wire args (Rust `src/bridge.rs`)**: `call_with_args(command, args)`
  adds an `"args"` object to the frame; plain `call()` still sends the
  U0 frame shape with no `"args"` key (covered by a new test asserting
  both shapes). Queue, deadline, and session rules unchanged.
- **Tools (Rust `src/main.rs`, v0.2.0)**: the U0 two plus
  `tackle_gameobject_properties` (path, validated: non-empty, ≤1024
  bytes, no empty segments), `tackle_console` (`max_entries` 1..500,
  omit for newest 100), `tackle_project_info`, `tackle_selection`.
  Hierarchy pagination: `offset` ≥ 0, `limit` 1..500, omit for the
  editor default (500). All argument validation runs in the MCP layer
  before any editor call (stdio test proves invalid args are rejected
  with no editor present). All six tools are `readOnlyHint: true`.
- **Resources (same file)**: `tackle://editor/state`,
  `tackle://editor/scene-hierarchy` (default page),
  `tackle://editor/project-info`, `tackle://editor/console`
  (newest 100), `tackle://editor/selection` — all `application/json`,
  served by the same bridge calls as the tools. Unknown URI →
  `-32002`; editor failure → `-32603` with the editor's message.
- **Editor commands (C# `TackleCommands.cs`)**: `gameobject_properties`
  (path walk by name, first match per level; transform, tag, layer,
  component type names), `console` (see §4), `project_info`
  (PlayerSettings + build target + build scenes, capped at 100),
  `selection` (selected GameObjects + active object + count of other
  selected objects). Hierarchy now walks then pages the walked list,
  so default calls match U0's response plus the new paging fields.
- **`TackleLifecycle.cs`** (new, Editor-only): `TackleLifecycle.Exit()`
  for `-executeMethod` graceful shutdowns (U0 lesson; see §4 for what
  actually stopped the editor in this run).

## 4. Deviations and findings (read these)

1. **Two missing commas, found live, fixed, re-gated.** The first
   `project_info` run returned malformed JSON (missing commas before
   `companyName` and `bundleVersion` in the hand-built builder). A
   malformed reply is a protocol violation and ends the session by
   design, so the following calls failed with `no editor connected`
   until the editor reconnected — that cascade is the protocol working,
   not a second bug. After the fix, the full smoke passed. Lesson
   carried: every new hand-built JSON object needs a live round-trip
   before it is trusted; the Rust side cannot catch a C# builder slip.
2. **`GetInstanceID()` is obsolete-as-error in 6000.6.6f1** (CS0619
   treated as an error: "Use GetEntityId instead"). The selection tool
   dropped the instance-id field instead of adopting an unverified new
   API: no U1 tool looks anything up by id, so the field carried no
   load. The tool description was updated to match.
3. **No resource mirror for `gameobject_properties`.** Its path
   argument contains `/` separators, which a single URI-template
   variable cannot express; the other five tools each have a static
   resource. This is a documented deviation from PROGRAM.md's
   "resources mirroring the tools", not an oversight.
4. **This editor instance does not auto-refresh running edits.**
   Copying fixed C# into the spike package while the editor ran did
   NOT recompile it — not on file change, not on window focus, not on
   Ctrl+R. The DLL timestamp proved the running assembly was stale
   through two smoke runs. The reliable trigger in this setup is an
   editor restart (startup import compiles). Smoke workflow rule going
   forward: change C#, restart the editor, verify the DLL timestamp
   moved, then run the smoke. (The first relaunch compiled clean with
   0 errors and no Safe Mode dialog.)
5. **Safe Mode dialog.** The first launch compiled the pre-fix C# with
   the CS0619 error and Unity blocked on an "Enter Safe Mode?" modal.
   Blind-accepting it would have been wrong; closing the editor's main
   window destroyed the window but left the process alive behind the
   modal, and closing the modal then exited the editor cleanly (no
   crash, normal shutdown lines in the log).
6. **windowclose does not reliably stop this editor.** On the later
   instance, closing the main window left the process alive spewing
   Gdk-CRITICAL cursor assertions; SIGTERM on the exact PID then
   segfaulted it (U0's finding, reproduced) and opened the Unity Bug
   Reporter. No report was sent: the reporter window was closed
   without submitting. Graceful stop remains an open U1 lesson:
   `TackleLifecycle.Exit()` exists for `-executeMethod` launches, but
   a running GUI editor still has no clean remote quit path proven
   here. (The editor was left stopped, not running, at the end of
   this lane — see §5.)
7. **U0's deferred items, carried not done.** Stable instance IDs
   (project-path hash + process role, for the import-worker ping-pong)
   and token rotation remain U1+ work; the worker guard and
   replace-on-reconnect semantics from U0 are unchanged. The
   `SERVERS.md` entry (server #11) and diver `declared_servers.lua`
   entry are Pax's gate landings per PROGRAM.md, not this lane's.
8. **Console `mode` is raw.** `LogEntry.mode` bit meanings are
   Unity-internal and were not guessed: the tool reports the integer
   and documents it as raw. Message bodies are truncated at 2000
   characters with a per-entry `messageTruncated` flag.

## 5. Files and state at handoff

Committed in `~/tackle` (local only, no remote, nothing pushed):
`Cargo.toml` (0.2.0), `src/{main,bridge,lib}.rs`,
`tests/{bridge,stdio}.rs`, `scripts/smoke.py`,
`unity/com.qompass.tackle/Editor/{TackleCommands.cs,TackleLifecycle.cs}`,
this report. Untracked and not mine: `scripts/spike-copy.sh` (left
untouched and uncommitted, as in U0).

Outside the repo: `~/tackle-spike/project` (the copy, with the embedded
package synced to the committed C#) and `~/tackle-spike/evidence/`
(`editor-u1.log` — first launch with the CS0619 error and Safe Mode
dialog; `editor-u1b.log` — clean relaunch + wedged quit;
`editor-u1c.log` — final clean launch + passing smoke;
`smoke-transcript-u1.json` — the passing run).

Original project untouched: `~/strobe-trackb` was never opened,
attached to, or modified (HEAD `6ae7f84e0a96`, status re-verified at
handoff). The editor used at handoff is stopped; no Unity process from
this lane remains running and no crash reporter was submitted.
