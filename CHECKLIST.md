---
repo: tackle
updated: 2026-10-21
---

# tackle — master checklist

Derived from the tackle program documents (estate package
`tackle-20261020/`: STUDY.md, PROGRAM.md, BRIEF-U0.md). Every item
traces to a stage gate in those documents. States: `[x]`
gated-closed · `[~]` delivered, ungated · `[ ]` open. The animated
tracker (assets/tracker.svg) is generated from this file only; it
moves only when an item's state changes at a gate.

## Ideation & commissioning

- [x] Editor-broker census delivered (STUDY.md — unity-mcp core surface, godot-ai core spec, 2026-10-20)
- [x] Program commissioned: a separate broker server from lumen; Unity backend first (Owner's ruling, 2026-10-20)
- [x] Name ruled: tackle (Q-EB-1 resolved, 2026-10-20)
- [x] Core contract fixed (authenticated hops with no fallback, explicit session routing, main-thread serialization, idempotent mutations, mandatory undo participation, flowguard labels on execute class)
- [x] GitHub repo created with master checklist + animated tracker (Owner's word, 2026-10-21; Q-EB-2)

## U0 — spike: prove the editor channel

- [ ] U0 run on primo against a throwaway copy of a project (never the Owner's live project state)
- [ ] Capability handshake over WebSocket proven (rotating tokens, loopback-only editor socket, capability dir permissions validated)
- [ ] Main-thread dispatch proven; editor_state + scene-hierarchy read round-tripping against Unity 6000.6.6f1
- [ ] U0 gate closed: WebSocket client story inside Unity's .NET profile, UPM local-package wiring, interactive-editor requirement — each resolved on evidence

## U1 — read core

- [ ] Read tools landed (editor_state, paginated scene hierarchy, GameObject properties, console read, project info, selection; read-only resources mirroring the tools)
- [ ] SERVERS.md census entry lands (estate server #11); diver declared-server entry added live-first after gates
- [ ] Registry record ai.qompass/tackle prepared (publish on Owner's word)

## U2 — write core

- [ ] Mutations landed (GameObject / component / scene, prefabs, script create/patch with compile-wait and diagnostics read-back) — undo-grouped and idempotent
- [ ] batch_execute atomic (one undo group, rollback on first error); bounded filesystem operations, delete = OS trash

## U3 — execute class

- [ ] Play-mode control + running-game inspection
- [ ] Test runner + player builds
- [ ] execute_code landed last and separately gated (flowguard decide() re-run at dispatch; per-session enable; disabled by default)

## G — Godot backend

- [ ] Godot backend started when a Godot project justifies it, not before (GDScript add-on, same broker core)
- [ ] Godot backend gated to the same core contract (authenticated hops, idempotency, undo participation)

## 1.0

- [ ] 1.0 — the Unity backend gated end to end (U0–U3): the first engine backend complete
- [ ] 1.0 release record: conformance entry current; registry publish on Owner's word

## Beyond 1.0

- [ ] Maintenance + conformance re-probes at each release (declared vs live diff)
- [ ] Second-engine parity: the Godot backend reaches the Unity contract bar
- [ ] Next-minor backlog triaged from field use (remote / multi-user hosting stays a non-goal unless re-ruled)
