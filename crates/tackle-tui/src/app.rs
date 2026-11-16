//! Application state machine and the two run loops.
//!
//! State transitions follow validate → prepare → commit → observe:
//! a workflow is *staged* (prepared, shown exactly, no facade call),
//! then *approved* (committed) or *aborted* (discarded). The rung-1
//! safety rule is enforced at staging time in [`App::stage`]:
//!
//! - U1 reads stage in plan mode and execute in auto mode.
//! - U2 writes/destructive stage in plan mode only; in auto mode
//!   staging is refused with a reason, nothing is staged, and the
//!   facade is never called. The refusal is audited.

use std::collections::VecDeque;
use std::io::{BufRead, Write};

use serde_json::{Value, json};
use tackle::decide::Class;

use crate::facade::TackleFacade;
use crate::screens::EditorView;
use crate::triggers::{AuditLog, TriggerRule};
use crate::validate::validate_payload;

/// Event lines retained for the Log screen.
pub const LOG_LINES_MAX: usize = 500;

/// Longest line-mode input line, in bytes.
pub const INPUT_LINE_BYTES_MAX: usize = 64 * 1024;

/// Audit entries shown in the Fleet panel tail.
pub const AUDIT_TAIL_COUNT: usize = 10;

/// The five screens (DESIGN §D2 core, tackle-shaped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Server status + editor connection state.
    Overview,
    /// Scene/object inspection over the U1 read core.
    Editor,
    /// Selectable operations with plan/auto controls.
    Workflows,
    /// Bounded live event log.
    Log,
    /// Fleet members, trigger rules, audit tail.
    Fleet,
}

impl Screen {
    /// All screens in tab order.
    pub const ALL: [Screen; 5] = [
        Screen::Overview,
        Screen::Editor,
        Screen::Workflows,
        Screen::Log,
        Screen::Fleet,
    ];

    /// Display title.
    pub fn title(self) -> &'static str {
        match self {
            Screen::Overview => "Overview",
            Screen::Editor => "Editor",
            Screen::Workflows => "Workflows",
            Screen::Log => "Log",
            Screen::Fleet => "Fleet",
        }
    }

    /// Next screen in tab order (wraps).
    pub fn next(self) -> Screen {
        let index = Screen::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Screen::ALL[(index + 1) % Screen::ALL.len()]
    }
}

/// Run control: plan stages for approval; auto executes reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Stage the call, show exact tool + args, wait for approval.
    Plan,
    /// Execute declared read operations directly. Writes refuse.
    Auto,
}

impl Mode {
    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Auto => "auto",
        }
    }
}

/// One workflow definition: tool, operator label, sample arguments.
#[derive(Debug, Clone)]
pub struct WorkflowDef {
    /// MCP tool name.
    pub tool: &'static str,
    /// Short operator-facing label.
    pub label: &'static str,
    /// Sample arguments shown when staging from the list.
    pub args: Value,
    /// Capability class from `tackle::decide::classify`.
    pub class: Class,
}

fn def(tool: &'static str, label: &'static str, args: Value) -> WorkflowDef {
    WorkflowDef {
        tool,
        label,
        args,
        class: tackle::decide::classify(tool).unwrap_or(Class::Write),
    }
}

/// The full workflow catalogue: the six U1 reads, then the twelve
/// U2 writes. Sample write args carry a placeholder idempotency key
/// so the staged JSON is exactly what approval would send.
pub fn workflow_defs() -> Vec<WorkflowDef> {
    vec![
        def("tackle_editor_state", "Editor state", json!({})),
        def("tackle_scene_hierarchy", "Scene hierarchy", json!({})),
        def(
            "tackle_gameobject_properties",
            "GameObject properties",
            json!({"path": "Root"}),
        ),
        def("tackle_selection", "Current selection", json!({})),
        def("tackle_console", "Console entries", json!({})),
        def("tackle_project_info", "Project info", json!({})),
        def(
            "tackle_gameobject_create",
            "Create GameObject",
            json!({"name": "NewObject", "idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_gameobject_modify",
            "Modify GameObject",
            json!({"path": "Root", "idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_gameobject_delete",
            "Delete GameObject",
            json!({"path": "Root", "idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_component_add",
            "Add component",
            json!({"path": "Root", "type": "UnityEngine.BoxCollider",
                    "idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_component_remove",
            "Remove component",
            json!({"path": "Root", "type": "UnityEngine.BoxCollider",
                    "idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_prefab_instantiate",
            "Instantiate prefab",
            json!({"path": "Assets/Prefabs/Sample.prefab",
                    "idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_batch_execute",
            "Batch execute",
            json!({"operations": [], "idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_script_create",
            "Create script",
            json!({"path": "NewScript.cs", "contents": "",
                    "idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_script_patch",
            "Patch script",
            json!({"path": "NewScript.cs", "old_text": "", "new_text": "",
                    "idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_scene_save",
            "Save scene",
            json!({"idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_undo",
            "Undo",
            json!({"idempotency_key": "tui-sample"}),
        ),
        def(
            "tackle_redo",
            "Redo",
            json!({"idempotency_key": "tui-sample"}),
        ),
    ]
}

/// A staged call awaiting approval: exact tool + args, as shown.
#[derive(Debug, Clone, PartialEq)]
pub struct StagedCall {
    /// Tool name.
    pub tool: String,
    /// Arguments object, exactly as approval would send it.
    pub args: Value,
    /// Capability class.
    pub class: Class,
}

/// What staging did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOutcome {
    /// Staged; no facade call has happened.
    Staged,
    /// Executed immediately (auto-mode read).
    Executed,
    /// Refused with a reason; nothing staged, no facade call.
    Refused(String),
}

/// One log event line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// started / progress / done / failed / staged / refused / info.
    pub kind: String,
    /// Human-readable detail.
    pub message: String,
}

/// The application: facade, screen state, staging, log, audit.
pub struct App<F: TackleFacade> {
    facade: F,
    screen: Screen,
    mode: Mode,
    selected: usize,
    staged: Option<StagedCall>,
    events: VecDeque<Event>,
    rules: Vec<TriggerRule>,
    audit: AuditLog,
    cached_hierarchy: Option<Value>,
    cache_stale: bool,
    last_state: Option<Value>,
}

impl<F: TackleFacade> App<F> {
    /// Build the app in plan mode on the Overview screen (the safe
    /// default: nothing executes without staging first).
    pub fn new(facade: F, rules: Vec<TriggerRule>) -> App<F> {
        App {
            facade,
            screen: Screen::Overview,
            mode: Mode::Plan,
            selected: 0,
            staged: None,
            events: VecDeque::new(),
            rules,
            audit: AuditLog::new(),
            cached_hierarchy: None,
            cache_stale: false,
            last_state: None,
        }
    }

    /// The facade (tests inspect the fake's recorded calls here).
    pub fn facade(&mut self) -> &mut F {
        &mut self.facade
    }

    /// Current screen.
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// Switch screens.
    pub fn set_screen(&mut self, screen: Screen) {
        self.screen = screen;
    }

    /// Current run mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Switch run mode.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Selected workflow index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Move the workflow selection, clamped to the catalogue.
    pub fn select(&mut self, index: usize) {
        self.selected = index.min(workflow_defs().len().saturating_sub(1));
    }

    /// The staged call, if any.
    pub fn staged(&self) -> Option<&StagedCall> {
        self.staged.as_ref()
    }

    /// The declared trigger rules.
    pub fn rules(&self) -> &[TriggerRule] {
        &self.rules
    }

    /// The trigger audit log.
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// Log lines, oldest first.
    pub fn events(&self) -> Vec<Event> {
        self.events.iter().cloned().collect()
    }

    fn push_event(&mut self, kind: &str, message: String) {
        self.events.push_back(Event {
            kind: kind.to_string(),
            message,
        });
        while self.events.len() > LOG_LINES_MAX {
            self.events.pop_front();
        }
    }

    /// Facade status for the Overview screen.
    pub fn status(&mut self) -> Result<Value, String> {
        self.facade.status().map_err(|e| e.to_string())
    }

    /// Stage one call. In plan mode this never calls the facade.
    /// In auto mode reads execute and U2 writes refuse (rung 1).
    pub fn stage(&mut self, tool: &str, args: Value) -> StageOutcome {
        let Some(class) = tackle::decide::classify(tool) else {
            let reason = format!("{tool}: unknown tool");
            self.push_event("refused", reason.clone());
            return StageOutcome::Refused(reason);
        };
        if class != Class::Read && self.mode == Mode::Auto {
            let reason = format!(
                "{tool}: U2 write-core operations are plan-mode only in rung 1; \
                 auto execution refused"
            );
            self.push_event("refused", reason.clone());
            self.audit
                .record("gate-refusal", "tui-mode-gate", &reason, None);
            return StageOutcome::Refused(reason);
        }
        if self.mode == Mode::Auto {
            return match self.execute(tool, args, class) {
                Ok(_) => StageOutcome::Executed,
                Err(reason) => StageOutcome::Refused(reason),
            };
        }
        self.staged = Some(StagedCall {
            tool: tool.to_string(),
            args,
            class,
        });
        self.push_event("staged", format!("{tool} staged for approval"));
        StageOutcome::Staged
    }

    /// Stage the currently selected workflow definition.
    pub fn stage_selected(&mut self) -> StageOutcome {
        let defs = workflow_defs();
        let Some(def) = defs.get(self.selected) else {
            return StageOutcome::Refused("no workflow selected".to_string());
        };
        let (tool, args) = (def.tool.to_string(), def.args.clone());
        self.stage(&tool, args)
    }

    /// Approve the staged call: the one path from staging to the
    /// facade. With nothing staged this is an honest refusal.
    pub fn approve(&mut self) -> Result<Value, String> {
        let Some(staged) = self.staged.take() else {
            return Err("nothing staged to approve".to_string());
        };
        self.execute(&staged.tool, staged.args, staged.class)
    }

    /// Abort the staged call: discarded, never executed.
    pub fn abort(&mut self) -> bool {
        match self.staged.take() {
            Some(staged) => {
                self.push_event("info", format!("{} aborted before approval", staged.tool));
                true
            }
            None => false,
        }
    }

    fn execute(&mut self, tool: &str, args: Value, _class: Class) -> Result<Value, String> {
        self.push_event("started", format!("{tool} started"));
        let raw = match self.facade.call(tool, args) {
            Ok(value) => value,
            Err(error) => {
                let message = error.to_string();
                self.push_event("failed", format!("{tool}: {message}"));
                if message.contains("no editor connected") {
                    self.cache_stale = self.cached_hierarchy.is_some();
                }
                return Err(message);
            }
        };
        let bounded = validate_payload(tool, &raw).inspect_err(|reason| {
            self.push_event("failed", reason.clone());
        })?;
        if tool == "tackle_scene_hierarchy" {
            self.cached_hierarchy = Some(bounded.clone());
            self.cache_stale = false;
        }
        if tool == "tackle_editor_state" {
            self.last_state = Some(bounded.clone());
        }
        self.push_event("done", format!("{tool} done"));
        Ok(bounded)
    }

    /// Refresh the Editor screen's data through the U1 read core.
    /// On disconnect the cache is marked stale, never refreshed
    /// silently and never presented as live afterwards.
    pub fn refresh_editor(&mut self) -> Result<(), String> {
        let state = self.execute("tackle_editor_state", json!({}), Class::Read)?;
        self.last_state = Some(state);
        if !self.facade.editor_connected() {
            self.cache_stale = self.cached_hierarchy.is_some();
            return Err("no editor connected".to_string());
        }
        self.execute("tackle_scene_hierarchy", json!({}), Class::Read)?;
        Ok(())
    }

    /// The Editor screen's view, with freshness decided here: live
    /// only when the facade reports a connection *now*.
    pub fn editor_view(&mut self) -> EditorView {
        if self.facade.editor_connected() {
            if let (Some(state), Some(hierarchy)) = (&self.last_state, &self.cached_hierarchy)
                && !self.cache_stale
            {
                return EditorView::Live {
                    state: state.clone(),
                    hierarchy: hierarchy.clone(),
                };
            }
            return EditorView::Empty;
        }
        match &self.cached_hierarchy {
            Some(hierarchy) => EditorView::Stale {
                hierarchy: hierarchy.clone(),
            },
            None => EditorView::Disconnected,
        }
    }

    /// Text lines for the current screen (shared by both run loops).
    pub fn screen_lines(&mut self) -> Vec<String> {
        match self.screen {
            Screen::Overview => {
                let status = self.status().unwrap_or(json!({"server": "down"}));
                let count = self.facade.tools().map_or(0, |tools| tools.len());
                crate::screens::overview_lines(&status, count, self.rules.len())
            }
            Screen::Editor => {
                let view = self.editor_view();
                crate::screens::editor_lines(&view)
            }
            Screen::Workflows => {
                let defs: Vec<(String, String, Class)> = workflow_defs()
                    .iter()
                    .map(|d| (d.tool.to_string(), d.label.to_string(), d.class))
                    .collect();
                crate::screens::workflow_lines(
                    &defs,
                    self.selected,
                    self.mode.label(),
                    self.staged.as_ref().map(|s| s.tool.as_str()),
                )
            }
            Screen::Log => self
                .events
                .iter()
                .map(|e| format!("[{}] {}", e.kind, e.message))
                .collect(),
            Screen::Fleet => {
                let tail = self.audit.tail(AUDIT_TAIL_COUNT);
                crate::screens::fleet_lines(&self.rules, &tail)
            }
        }
    }

    /// Execute one line-mode command line; returns the text to
    /// print. Shares the staging path with the full-screen UI.
    pub fn execute_line(&mut self, line: &str) -> String {
        let line = line.trim();
        let (command, rest) = match line.split_once(' ') {
            Some((head, tail)) => (head, tail.trim()),
            None => (line, ""),
        };
        match command {
            "help" => HELP_TEXT.to_string(),
            "status" => self
                .status()
                .map(|v| v.to_string())
                .unwrap_or_else(|e| format!("error: {e}")),
            "tools" => match self.facade.tools() {
                Ok(tools) => tools
                    .iter()
                    .filter_map(|t| t.get("name").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => format!("error: {e}"),
            },
            "editor" | "refresh" => {
                if command == "refresh"
                    && let Err(e) = self.refresh_editor()
                {
                    return format!("error: {e}");
                }
                let view = self.editor_view();
                crate::screens::editor_lines(&view).join("\n")
            }
            "workflows" => {
                self.set_screen(Screen::Workflows);
                self.screen_lines().join("\n")
            }
            "mode" => {
                match rest {
                    "plan" => self.set_mode(Mode::Plan),
                    "auto" => self.set_mode(Mode::Auto),
                    _ => return format!("error: unknown mode `{rest}` (plan|auto)"),
                }
                format!("mode: {}", self.mode.label())
            }
            "run" => self.line_run(rest),
            "approve" => match self.approve() {
                Ok(value) => value.to_string(),
                Err(e) => format!("error: {e}"),
            },
            "abort" => {
                if self.abort() {
                    "aborted".to_string()
                } else {
                    "nothing staged".to_string()
                }
            }
            "log" => self
                .events
                .iter()
                .map(|e| format!("[{}] {}", e.kind, e.message))
                .collect::<Vec<_>>()
                .join("\n"),
            "fleet" => {
                self.set_screen(Screen::Fleet);
                self.screen_lines().join("\n")
            }
            "screen" => {
                let screen = match rest {
                    "overview" => Screen::Overview,
                    "editor" => Screen::Editor,
                    "workflows" => Screen::Workflows,
                    "log" => Screen::Log,
                    "fleet" => Screen::Fleet,
                    _ => return format!("error: unknown screen `{rest}`"),
                };
                self.set_screen(screen);
                self.screen_lines().join("\n")
            }
            "" => String::new(),
            other => format!("error: unknown command `{other}` (try `help`)"),
        }
    }

    fn line_run(&mut self, rest: &str) -> String {
        let (tool, args_text) = match rest.split_once(' ') {
            Some((head, tail)) => (head, tail.trim()),
            None => (rest, ""),
        };
        if tool.is_empty() {
            return "error: run needs a tool name".to_string();
        }
        let args = if args_text.is_empty() {
            default_args(tool)
        } else {
            match serde_json::from_str::<Value>(args_text) {
                Ok(value) if value.is_object() => value,
                Ok(_) => return "error: args must be a JSON object".to_string(),
                Err(e) => return format!("error: bad args JSON: {e}"),
            }
        };
        match self.stage(tool, args) {
            StageOutcome::Staged => {
                let staged = self.staged().expect("just staged");
                format!(
                    "staged {} {} — `approve` to execute, `abort` to discard",
                    staged.tool, staged.args
                )
            }
            StageOutcome::Executed => "executed".to_string(),
            StageOutcome::Refused(reason) => format!("refused: {reason}"),
        }
    }
}

fn default_args(tool: &str) -> Value {
    workflow_defs()
        .iter()
        .find(|d| d.tool == tool)
        .map(|d| d.args.clone())
        .unwrap_or_else(|| json!({}))
}

/// Line-mode help text.
pub const HELP_TEXT: &str = "commands: status | tools | editor | refresh | workflows | \
     mode plan|auto | run <tool> [json-args] | approve | abort | log | fleet | \
     screen <name> | help | quit";

/// What the line loop should do with one raw input line.
#[derive(Debug, PartialEq, Eq)]
pub enum LineAction {
    /// End the session.
    Quit,
    /// Ignore the line.
    Skip,
    /// Execute the trimmed text.
    Execute(String),
}

/// Classify one raw line-mode line. Pure; unit-tested.
pub fn classify_line(line: &str) -> LineAction {
    let text = line.trim();
    if text.is_empty() {
        LineAction::Skip
    } else if text == "quit" || text == "exit" {
        LineAction::Quit
    } else {
        LineAction::Execute(text.to_string())
    }
}

/// Run the non-TTY line mode over any reader/writer pair (pipes and
/// tests included): one command in, its text out, until quit/EOF.
/// An overlong line fails the session rather than buffering forever.
pub fn run_line_mode<F: TackleFacade, R: BufRead, W: Write>(
    app: &mut App<F>,
    reader: &mut R,
    writer: &mut W,
) -> std::io::Result<()> {
    writeln!(writer, "tackle-tui (line mode) — {HELP_TEXT}")?;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.len() > INPUT_LINE_BYTES_MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "input line exceeds byte bound",
            ));
        }
        match classify_line(&line) {
            LineAction::Quit => return Ok(()),
            LineAction::Skip => continue,
            LineAction::Execute(text) => {
                let output = app.execute_line(&text);
                if !output.is_empty() {
                    writeln!(writer, "{output}")?;
                }
                writer.flush()?;
            }
        }
    }
}

/// Run the full-screen ratatui UI (TTY only; `main` dispatches).
/// Keys: Tab next screen, 1–5 direct, j/k select, Enter stage,
/// y approve, n abort, p/a mode, r refresh editor, q quit.
pub fn run_fullscreen<F: TackleFacade>(app: &mut App<F>) -> std::io::Result<()> {
    use crossterm::event::{self, Event as CtEvent, KeyCode};
    use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;

    let mut stdout = std::io::stdout();
    terminal::enable_raw_mode()?;
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = loop {
        let title = app.screen().title().to_string();
        let lines = app.screen_lines();
        let tabs: Vec<&str> = Screen::ALL.iter().map(|s| s.title()).collect();
        let active = Screen::ALL
            .iter()
            .position(|s| *s == app.screen())
            .unwrap_or(0);
        let mut body = vec![crate::panels::tab_line(&tabs, active), String::new()];
        body.extend(lines);
        let drawn = term.draw(|frame| {
            let panel = crate::panels::text_panel(&body, &title, ratatui::style::Color::Cyan);
            frame.render_widget(panel, frame.area());
        });
        if let Err(e) = drawn {
            break Err(e);
        }
        if !event::poll(std::time::Duration::from_millis(100))? {
            continue;
        }
        let CtEvent::Key(key) = event::read()? else {
            continue;
        };
        match key.code {
            KeyCode::Char('q') => break Ok(()),
            KeyCode::Tab => app.set_screen(app.screen().next()),
            KeyCode::Char(digit @ '1'..='5') => {
                let index = usize::from(digit as u8 - b'1');
                app.set_screen(Screen::ALL[index]);
            }
            KeyCode::Char('j') => app.select(app.selected() + 1),
            KeyCode::Char('k') => app.select(app.selected().saturating_sub(1)),
            KeyCode::Char('p') => app.set_mode(Mode::Plan),
            KeyCode::Char('a') => app.set_mode(Mode::Auto),
            KeyCode::Char('r') => {
                let _ = app.refresh_editor();
                app.set_screen(Screen::Editor);
            }
            KeyCode::Enter => {
                app.stage_selected();
                app.set_screen(Screen::Workflows);
            }
            KeyCode::Char('y') => {
                let _ = app.approve();
                app.set_screen(Screen::Log);
            }
            KeyCode::Char('n') => {
                app.abort();
            }
            _ => {}
        }
    };
    terminal::disable_raw_mode()?;
    crossterm::execute!(term.backend_mut(), LeaveAlternateScreen)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facade::FakeFacade;

    fn app_with(fake: FakeFacade) -> App<FakeFacade> {
        App::new(fake, Vec::new())
    }

    #[test]
    fn ok_plan_mode_stages_read_without_calling() {
        let mut app = app_with(FakeFacade::sample());
        let outcome = app.stage("tackle_editor_state", json!({}));
        assert_eq!(outcome, StageOutcome::Staged);
        assert!(app.facade().calls.is_empty(), "plan mode must not call");
        assert_eq!(app.staged().expect("staged").tool, "tackle_editor_state");
    }

    #[test]
    fn ok_approve_executes_the_staged_call_exactly_once() {
        let mut app = app_with(FakeFacade::sample());
        app.stage("tackle_editor_state", json!({}));
        app.approve().expect("approve");
        assert_eq!(app.facade().calls.len(), 1);
        assert!(app.staged().is_none());
    }

    #[test]
    fn ok_auto_mode_read_executes_directly() {
        let mut app = app_with(FakeFacade::sample());
        app.set_mode(Mode::Auto);
        let outcome = app.stage("tackle_selection", json!({}));
        assert_eq!(outcome, StageOutcome::Executed);
        assert_eq!(app.facade().calls.len(), 1);
    }

    #[test]
    fn adv_u2_write_in_auto_mode_is_refused_without_call() {
        let mut app = app_with(FakeFacade::sample());
        app.set_mode(Mode::Auto);
        for tool in [
            "tackle_gameobject_create",
            "tackle_gameobject_delete",
            "tackle_script_patch",
            "tackle_scene_save",
            "tackle_undo",
            "tackle_batch_execute",
        ] {
            match app.stage(tool, json!({"idempotency_key": "k"})) {
                StageOutcome::Refused(reason) => {
                    assert!(reason.contains("plan-mode only"), "{tool}: {reason}");
                }
                other => panic!("{tool}: expected refusal, got {other:?}"),
            }
        }
        assert!(app.facade().calls.is_empty(), "refusals must not call");
        assert!(app.staged().is_none());
        assert!(!app.audit().is_empty(), "refusals are audited");
    }

    #[test]
    fn ok_u2_write_stages_in_plan_then_executes_on_approve() {
        let mut app = app_with(FakeFacade::sample());
        let outcome = app.stage(
            "tackle_gameobject_create",
            json!({"name": "X", "idempotency_key": "k"}),
        );
        assert_eq!(outcome, StageOutcome::Staged);
        assert!(app.facade().calls.is_empty());
        app.approve().expect("approve");
        assert_eq!(app.facade().calls.len(), 1);
    }

    #[test]
    fn adv_approval_then_abort_never_executes() {
        let mut app = app_with(FakeFacade::sample());
        app.stage("tackle_scene_save", json!({"idempotency_key": "k"}));
        assert!(app.abort());
        assert!(app.approve().is_err());
        assert!(app.facade().calls.is_empty());
    }

    #[test]
    fn adv_unknown_tool_is_refused_without_call() {
        let mut app = app_with(FakeFacade::sample());
        app.set_mode(Mode::Auto);
        assert!(matches!(
            app.stage("tackle_nope", json!({})),
            StageOutcome::Refused(_)
        ));
        assert!(app.facade().calls.is_empty());
    }

    #[test]
    fn adv_disconnected_editor_never_shows_cache_as_live() {
        let mut fake = FakeFacade::sample();
        fake.script(
            "tackle_scene_hierarchy",
            json!({"totalCount": 0, "nodes": []}),
        );
        let mut app = app_with(fake);
        app.refresh_editor().expect("connected refresh");
        assert!(matches!(app.editor_view(), EditorView::Live { .. }));
        // The editor drops: facade now reports disconnected + errors.
        app.facade().connected = false;
        app.facade().call_error = Some("no editor connected".to_string());
        assert!(app.refresh_editor().is_err());
        match app.editor_view() {
            EditorView::Stale { .. } => {}
            other => panic!("expected stale view, got {other:?}"),
        }
        let lines = crate::screens::editor_lines(&app.editor_view());
        assert!(lines[0].contains("STALE"));
    }

    #[test]
    fn adv_malformed_editor_payload_is_bounded_not_rendered_raw() {
        let mut fake = FakeFacade {
            connected: true,
            ..FakeFacade::default()
        };
        fake.script("tackle_scene_hierarchy", json!(["not-an-object"]));
        let mut app = app_with(fake);
        app.set_mode(Mode::Auto);
        // Validation refuses the wrong shape; the call happened but
        // the payload never reaches a screen as a hierarchy.
        let outcome = app.stage("tackle_scene_hierarchy", json!({}));
        assert!(matches!(outcome, StageOutcome::Refused(_)));
    }

    #[test]
    fn ok_line_mode_answers_status_from_a_pipe() {
        let mut app = app_with(FakeFacade::sample());
        let mut input: &[u8] = b"status\nquit\n";
        let mut output: Vec<u8> = Vec::new();
        run_line_mode(&mut app, &mut input, &mut output).expect("line mode");
        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("tackle"));
    }

    #[test]
    fn ok_line_run_plan_flow_end_to_end() {
        let mut app = app_with(FakeFacade::sample());
        let staged = app.execute_line("run tackle_editor_state");
        assert!(staged.contains("staged"));
        assert!(app.facade().calls.is_empty());
        let done = app.execute_line("approve");
        assert!(!done.starts_with("error"));
        assert_eq!(app.facade().calls.len(), 1);
    }

    #[test]
    fn adv_line_run_u2_in_auto_is_refused() {
        let mut app = app_with(FakeFacade::sample());
        app.execute_line("mode auto");
        let out = app.execute_line("run tackle_gameobject_delete");
        assert!(out.contains("plan-mode only"), "{out}");
        assert!(app.facade().calls.is_empty());
    }

    #[test]
    fn ok_classify_line_variants() {
        assert_eq!(classify_line("  quit "), LineAction::Quit);
        assert_eq!(classify_line("   "), LineAction::Skip);
        assert_eq!(
            classify_line("status"),
            LineAction::Execute("status".to_string())
        );
    }

    #[test]
    fn ok_workflow_catalogue_is_all_18_tools_classified() {
        let defs = workflow_defs();
        assert_eq!(defs.len(), 18);
        let reads = defs.iter().filter(|d| d.class == Class::Read).count();
        assert_eq!(reads, 6);
    }

    #[test]
    fn adv_first_screen_renders_under_test_backend() {
        let mut app = app_with(FakeFacade::sample());
        let lines = app.screen_lines();
        let panel = crate::panels::text_panel(&lines, "Overview", ratatui::style::Color::Cyan);
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| frame.render_widget(panel, frame.area()))
            .expect("draw");
    }
}
