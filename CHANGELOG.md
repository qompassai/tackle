# Changelog

## 0.4.1 — 2026-11-27

- crates.io README fix: the 0.4.0 publish's registry metadata carried the README filename instead of its content, so crates.io rendered the literal text "README.md" and the MCP Registry ownership check could not find the `mcp-name:` marker. This patch re-publishes with the README content carried correctly. No code or tool-surface change.

## 0.4.0 — 2026-11-27

- First publication to the MCP Registry as `ai.qompass/tackle` (Registry onboarding scoped by the release sweep record, ~/workspace/release-sweep-20261127/RECORD.md, held item 7). server.json aligned to the crate version (0.4.0, cargo package `tackle-mcp`, stdio).
- This changelog begins at 0.4.0; earlier history lives in REPORT.md, REPORT-U1.md and CHECKLIST.md.
- No tool-surface or behaviour change from 0.3.2.
