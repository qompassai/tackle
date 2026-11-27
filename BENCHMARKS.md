# Benchmarks — tackle

Measured 2026-11-27 on primo: 13th Gen Intel Core i7-13620H (16 threads), 62 GiB RAM, cargo 1.100.0, server binary driven over stdio by `benches/hot_paths.rs` (hand-rolled on `std::time`; warmup 20, measured loops as stated). Primo was under extreme parallel load during this run (load average ~65–100 from concurrent fleet work), so these numbers are inflated versus an idle machine — treat them as order-of-magnitude baselines, not lab numbers.

## Workloads

- `cold_start_discover` — spawn the server (fresh `TACKLE_STATE_DIR`, free bridge port), send `server/discover`, await the correlated response; 10 spawns.
- `tools_list` — `tools/list` over one warm session; 200 iterations.
- `tool_call_tackle_editor_state` — read-only `tackle_editor_state` call over the same warm session with no editor attached (the honest no-editor error path); 200 iterations.

## Results (2026-11-27)

| workload | iters | total | per-op | ops/s |
|---|---|---|---|---|
| cold_start_discover | 10 | 1357.1 ms | 135.71 ms | 7 |
| tools_list | 200 | 590.5 ms | 2952.7 µs | 339 |
| tool_call_tackle_editor_state | 200 | 231.7 ms | 1158.7 µs | 863 |

## Example

`examples/stdio_session.rs` — one modern-era session (discover, tools/list, `tackle_editor_state` with no editor attached). Run: `cargo run --example stdio_session`.
