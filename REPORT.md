# tackle U0 spike — REPORT (2026-10-21, primo)

**Verdict: feasible.** A Rust MCP server (rmcp, stdio) held an authenticated
WebSocket session with a live Unity 6000.6.6f1 Editor on a throwaway copy of
strobe-trackb. Both read tools returned real data, executed on the Editor main
thread. One real bug turned up and was fixed: AssetImportWorker processes
hijacked the session (see §4).

## 1. Gates (all run on primo)

| Gate | Result |
|---|---|
| `cargo test` | **28 passed, 0 failed** (bridge 15, capability 8, stdio 5; 0 unit/doc). Split: 11 validation (`ok_`) / 17 adversarial (`adv_`) |
| `cargo fmt --check` | clean (exit 0, no output) |
| `cargo clippy --all-targets -- -D warnings` | clean (no diagnostics) |
| `nix build` | success → `/nix/store/p7nw34gx18vh3xmxshb6shj41rkkxffw-tackle-0.1.0`; its check phase re-ran the suite (stdio 5/5 shown in log) |
| C# compile | `Tackle.Editor.dll` built (Csc, ILPostProcess, CopyFiles to `Library/ScriptAssemblies`); Editor log has **0 `error CS`** lines and 22 `warning CS` lines, **0 of them in the Tackle package** (all CS0618 in the project's own `TestAnim` scripts, pre-existing) |
| Session connected | Editor log: `[Tackle] session connected to 127.0.0.1:9770 via System.Net.WebSockets.ClientWebSocket`. Server: `editor session 1 connected from 127.0.0.1:46882` |
| Both tools, real data | `scripts/smoke.py` against `result/bin/tackle`: `connected=True after 3 polls; editor_state error=False; scene_hierarchy error=False` |
| Original untouched | `original_manifest.py compare`: files before 135, after 135; HEAD `6ae7f84e0a96` equal; `git status` (37 dirty lines, Matt's TestAnim WIP) equal; **identical True** |

The adversarial tests cover: wrong token, missing token, token prefix and
suffix variants, lowercase `bearer`, a bare token, an empty token, a browser
`Origin` header sent with a valid token (403), a wrong path (404), garbage bytes
instead of an HTTP upgrade, an oversized (>1 MiB) message, a malformed reply,
four reply-shape violations (unsent id, missing result, both result and error,
an unknown field), no editor (library and both MCP tools over stdio), a
wrong-or-missing token refused **and logged without the token appearing in the
log** (real binary), a loose (0755) capability dir that is refused and not
repaired, a symlinked capability dir, bad ports or a relative state dir, and
port-in-use (exits with no stdout and publishes no capability file).

Port 9770 was free before binding (`ss -ltnp`: no listener on 9770). No other
Unity Editor was running, so only the spike instance was ever launched.

## 2. Round-trip transcript (run 2, the passing run)

Full JSON: `~/tackle-spike/evidence/smoke-transcript.json`. Server stderr, verbatim:

```
12:18:41 tackle: v0.1.0 listening on 127.0.0.1:9770; capability file /home/phaedrus/.local/state/tackle/capability/editor.json
12:18:41 tackle: editor_state: no editor connected
12:18:41 tackle: scene_hierarchy: no editor connected
12:18:41 tackle: editor_state: no editor connected
12:18:44 tackle: editor_state: no editor connected
12:18:46 tackle: editor session 1 connected from 127.0.0.1:46882
12:18:48 tackle: editor_state: ok
12:18:48 tackle: editor_state: ok
12:18:49 tackle: scene_hierarchy: ok
```

`initialize` → serverInfo `{"name": "tackle", "title": "Tackle", "version": "0.1.0"}`;
`tools/list` → `tackle_editor_state`, `tackle_scene_hierarchy` (both readOnlyHint).
Before the editor connected, both tools returned `isError: true` with the text `no editor connected`.

`tackle_editor_state` (live, isError=false):
```json
{"activeScene": {"name": "TestAnim", "path": "Assets/Scenes/TestAnim.unity"},
 "isCompiling": false, "isPlaying": false, "onMainThread": true,
 "productName": "strobe-trackb", "projectName": "project",
 "projectPath": "/home/phaedrus/tackle-spike/project", "queueDepth": 0,
 "unityVersion": "6000.6.6f1"}
```

`tackle_scene_hierarchy` (live, isError=false; preorder, depth from 0):
```json
{"scene": {"name": "TestAnim", "path": "Assets/Scenes/TestAnim.unity"},
 "rootCount": 4, "depthMax": 4, "nodeCountMax": 500, "nodeCount": 11,
 "truncatedByDepth": true, "truncatedByCount": false, "onMainThread": true,
 "nodes": [
  {"name": "Ground", "active": true, "childCount": 0, "depth": 0},
  {"name": "Key Light", "active": true, "childCount": 0, "depth": 0},
  {"name": "Main Camera", "active": true, "childCount": 0, "depth": 0},
  {"name": "Seraphine (factory)", "active": true, "childCount": 3, "depth": 0},
  {"name": "BaseMesh", "active": true, "childCount": 0, "depth": 1},
  {"name": "Icosphere", "active": true, "childCount": 0, "depth": 1},
  {"name": "Root", "active": true, "childCount": 1, "depth": 1},
  {"name": "Hips", "active": true, "childCount": 3, "depth": 2},
  {"name": "LeftUpLeg", "active": true, "childCount": 1, "depth": 3},
  {"name": "RightUpLeg", "active": true, "childCount": 1, "depth": 3},
  {"name": "Spine", "active": true, "childCount": 1, "depth": 3}]}
```
The depth cap fired on real data: the depth-3 leg and spine bones have children
that were not walked, and `truncatedByDepth` is true. `onMainThread: true` is
the marshalling proof. The command compares its thread against the id captured
by the `[InitializeOnLoad]` static constructor, and `SceneManager` calls would
throw if made off the main thread.

## 3. What was built

- **Rust crate** (`src/`): `config.rs` (env: `TACKLE_PORT` default 9770, ≥1024;
  state dir `TACKLE_STATE_DIR` → `$XDG_STATE_HOME/tackle` → `~/.local/state/tackle`);
  `capability.rs` (256-bit token from `/dev/urandom`, `Debug` redacted,
  constant-time compare; `capability/` dir created 0700 and validated (no symlink,
  no group/world bits, refused rather than repaired); `editor.json`
  `{"version":1,"port","token"}` written 0600 via temp + rename, published only
  after the port is bound); `bridge.rs` (bind 127.0.0.1 only; upgrade on
  `/editor` with `Authorization: Bearer`; refusals logged and answered
  401/403/404; 5 s handshake deadline; 1 MiB message/frame cap; one command in
  flight; bounded queue of 8, full → `editor busy`; 15 s call deadline; stale
  replies discarded by id; malformed replies end the session); `main.rs` (rmcp
  `ServerConfig` + `Implementation::new("tackle", env!("CARGO_PKG_VERSION")).with_title("Tackle")`,
  the twilight mechanism). Flake: `packages.default` + `devShells.default`.
- **Unity package** `com.qompass.tackle` (asmdef `Tackle.Editor`, Editor-only):
  canonical source in `unity/com.qompass.tackle/`, installed as an embedded
  package in `~/tackle-spike/project/Packages/`. `TackleBridge.cs` runs a
  background socket loop that reads the capability file and connects with
  reconnect every 2 s, logging only state changes. The loop does not read the
  next command until the current reply has gone out. `EditorApplication.update`
  pumps one command per tick in arrival order. `TackleCommands.cs` holds
  `SceneDepthMax = 4` and `SceneNodeCountMax = 500` as named constants and walks
  the hierarchy with an explicit stack (no recursion).
- **ClientWebSocket verdict:** `System.Net.WebSockets.ClientWebSocket` exists
  and works in 6000.6.6f1 (Mono). `SetRequestHeader("Authorization", …)` was
  honoured, so **nothing was vendored**.

## 4. Deviations and findings (read these)

1. **Run 1 failed, and the cause was found and fixed.** The session connected,
   then the server saw *pairs* of connections every ~2 s replacing each other,
   and `scene_hierarchy` timed out (15 s). Unity 6's **AssetImportWorker**
   processes (`Unity -batchMode -name AssetImportWorkerHW0/1/2`) load editor
   assemblies and so run `[InitializeOnLoad]`. Each one connected with the valid
   token and never pumped commands. Evidence: `[Tackle]` lines in
   `AssetImportWorkerHW1.log` (11) and `HW2.log` (9), none of them visible in
   the main Editor log. Fix: `AssetDatabase.IsAssetImportWorkerProcess()` →
   stay inert. I also made `Stop()` (before reload or quit) call
   `ClientWebSocket.Abort()` synchronously. In run 2 there was one session, and
   the current worker logs show 0 `[Tackle]` lines. Run-1 evidence was kept:
   `evidence/editor-run1.log`, `smoke-transcript-run1.json`, `run1-worker-logs/`.
   **U1 lesson:** "newest connection replaces" let two same-token clients
   ping-pong. U1's stable instance IDs (project path hash + process role) should
   make that impossible rather than just unlikely.
2. **Stopping the Editor with SIGTERM segfaulted it** (background exit 139, then
   144) and opened a **UnityBugReporter crash dialog on DISPLAY :0** each time. I
   killed both dialogs; no report was sent by me. Orphaned import workers exited
   on their own. U1 needs a graceful stop (for example an `-executeMethod` that
   calls `EditorApplication.Exit(0)`), not a signal.
3. **Copy step:** the profile permits neither `bash` nor `rm`, so I did not run
   the staged `scripts/spike-copy.sh`. I did the same thing with Python
   `shutil.copytree(symlinks=True)` excluding top-level `Temp/` (the lockfile).
   The before/after manifest uses my `scripts/original_manifest.py`, which
   excludes Library/Temp/Logs like the staged script. `spike-copy.sh` was
   pre-existing and untracked; I left it untouched and uncommitted.
4. **No `rust-toolchain.toml` pin.** This mirrors twilight; the Tiger Style
   skill asks for a nightly pin. Local builds use the system rustc 1.99.1.
   `flake.lock` reuses twilight's nixpkgs pin (rev `09063bd1`) so nix used a
   store-cached nixpkgs.
5. **Auth at the HTTP upgrade, not in a first message.** An unauthenticated
   peer never gets a WebSocket. Any `Origin` header is refused, because browser
   pages can open loopback WebSockets and the token is the only other guard.
6. **Simplified from PROGRAM's contract (U1 work, as briefed):** no token
   rotation; no ancestor-directory permission walk; the capability dir owner
   is compared against the file we just wrote, not against the process uid; a
   stale `editor.json` stays after exit (the next run overwrites it). The
   accept loop has no concurrent-connection cap; each peer is bounded only by
   the 5 s handshake deadline.
7. **Capability file stays on disk** at `~/.local/state/tackle/capability/editor.json`
   (0600, dead token from the smoke run). I did not delete it because `rm` is
   not permitted. It is harmless: nothing listens on 9770 now.

## 5. Transport evaluation and U1 recommendation

Caveat: I did **not** fetch butterlatte-zhang/unity-ai-bridge. This comparison
uses STUDY.md's summary of it (file-based IPC, zero dependencies, polling)
plus what the spike hit.

| Question | WebSocket (observed) | File IPC (per STUDY.md, reasoned) |
|---|---|---|
| Client dependency in 6000.6.6f1 | none: built-in ClientWebSocket worked, 0 vendored bytes | none |
| Auth wiring | ~25 lines Rust (`authorize`) plus one C# header line; refusals are explicit and logged per peer (401/403/404) | only filesystem permissions; no peer identity, so there is nothing to refuse or log |
| Latency | push; connect 5 s after server start (reconnect cadence); editor_state→scene_hierarchy replies 1 s apart, bounded by `update` ticks | poll interval plus the same `update` tick; the main-thread pump is identical either way |
| Domain reload | socket dies; needs reconnect + abort-on-reload (built, observed) | queue survives reloads on disk, a real advantage for commands issued mid-reload |
| Import-worker hazard (hit) | workers connected and stole the session; fixed with a one-line guard | **worse:** workers would poll the same command directory as competing consumers and could silently claim commands; the same guard is needed, and failures are silent instead of logged |
| Liveness / "no editor" | exact: socket present or not (`no editor connected`) | inferred from heartbeat-file staleness |

**Recommendation: lock WebSocket for U1.** Reasons from the spike: it needed
zero dependencies on this .NET profile; it gives explicit, logged refusal of
unauthenticated peers, which PROGRAM contract 1 ("no unauthenticated mode")
requires and file IPC cannot express; and "is an editor connected" is exact.
The hazard that actually bit (import workers) affects file IPC at least as
badly. File IPC's one real win, surviving domain reloads, is better handled by
contract item 4: idempotency keys plus client retry across reconnect. For U1,
carry forward the worker guard, abort-on-reload, a graceful-shutdown path, and
role-aware instance IDs.

## 6. Files

Committed in `~/tackle` (local only, no remote, nothing pushed): `Cargo.toml`,
`Cargo.lock`, `flake.nix`, `flake.lock`, `.gitignore`, `src/{lib,config,capability,bridge,main}.rs`,
`tests/{bridge,capability,stdio}.rs`, `tests/common/mod.rs`,
`unity/com.qompass.tackle/{package.json,Editor/Tackle.Editor.asmdef,Editor/TackleBridge.cs,Editor/TackleCommands.cs}`,
`scripts/{original_manifest.py,smoke.py}`, `REPORT.md`. Untracked and not mine:
`scripts/spike-copy.sh`.

Outside the repo: `~/tackle-spike/project` (the copy, with the embedded package
and Unity-generated `.meta` files) and `~/tackle-spike/evidence/` (`editor.log`,
`editor-run1.log`, `smoke-transcript*.json`, `run1-worker-logs/`,
`original.{before,after}.json`).

**Resume notes for U1:** the Rust package does not version the C# package; the
two copies must be kept in sync by hand (`diff -r` against the repo). Unity
generated `.meta` files only in the spike copy, so the repo package has none
yet.
