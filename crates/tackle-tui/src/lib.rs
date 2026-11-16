//! `tackle-tui` — the Unity-editor broker console for tackle
//! (fleet TUI programme D-1562, rung 1).
//!
//! The crate implements the fleet TUI pattern (DESIGN §D2): the UI
//! programs against the narrow [`facade::TackleFacade`] trait, panels
//! are pure widget constructors, and the same execute path drives the
//! full-screen ratatui UI and the non-TTY line mode.
//!
//! # Rung-1 scope and safety shape
//!
//! - Screens: Overview, Editor (scene/object inspection over the U1
//!   read core), Workflows, Log/Live, Fleet (trigger rules visible,
//!   audit present; no outbound rules ship in rung 1).
//! - U1 read workflows run in plan or auto mode. **U2 write-core
//!   operations are plan-mode only**: staging one in auto mode is
//!   refused with a reason and no facade call happens (see
//!   [`app::App::stage`]). This mirrors the broker's own caution:
//!   writes additionally pass tackle's `decide()` gate server-side.
//! - Editor honesty: a cached scene is only presented as live while
//!   the facade reports the editor connected; after a disconnect the
//!   Editor screen shows the cache explicitly marked stale.
//! - No real Unity editor is ever required: every test runs against
//!   [`facade::FakeFacade`] and tackle's own fixtures.
//!
//! # Limits (named, with units)
//!
//! - [`app::LOG_LINES_MAX`] event lines, [`app::INPUT_LINE_BYTES_MAX`]
//!   bytes per line-mode input line.
//! - Editor payloads are bounded by [`validate`]: serialized bytes,
//!   nesting depth, array items, object keys, and text characters all
//!   carry named maxima; oversized text/arrays are truncated with a
//!   marker, wrong shapes are refused.
//! - The trigger audit is bounded by [`triggers::AUDIT_LINES_MAX`].

#![forbid(unsafe_code)]

pub mod app;
pub mod facade;
pub mod panels;
pub mod screens;
pub mod triggers;
pub mod validate;

pub use app::{App, Mode, Screen, StageOutcome};
pub use facade::{FacadeError, FakeFacade, LiveFacade, TackleFacade};
pub use triggers::{AuditLog, TriggerRule, load_rules};
