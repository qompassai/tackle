//! tackle-tui — thin binary: parse args, build the real facade,
//! run. All logic lives in the `tackle_tui` library crate.
//!
//! Usage: `tackle-tui [--line] [--fake] [--server-bin <path>]`
//! - `--fake` drives the in-memory FakeFacade (demos, smoke tests);
//!   no server is spawned and no editor is contacted.
//! - Otherwise the LiveFacade spawns the `tackle` server binary
//!   (default: `tackle` on PATH, or `$TACKLE_TUI_SERVER_BIN`).
//! - Full-screen ratatui runs on a TTY; otherwise line mode.

#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use tackle_tui::app::{run_fullscreen, run_line_mode};
use tackle_tui::{App, FakeFacade, LiveFacade, TackleFacade, load_rules};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: tackle-tui [--line] [--fake] [--server-bin <path>]");
        return ExitCode::SUCCESS;
    }
    let force_line = args.iter().any(|a| a == "--line");
    let use_fake = args.iter().any(|a| a == "--fake");
    let server_bin = arg_value(&args, "--server-bin")
        .or_else(|| std::env::var_os("TACKLE_TUI_SERVER_BIN").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("tackle"));
    let rules = load_rules(&triggers_path());
    if use_fake {
        run_with(App::new(FakeFacade::sample(), rules), force_line)
    } else {
        run_with(App::new(LiveFacade::new(server_bin), rules), force_line)
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<PathBuf> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| PathBuf::from(&pair[1]))
}

fn triggers_path() -> PathBuf {
    std::env::var_os("TACKLE_TUI_TRIGGERS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("triggers.toml"))
}

fn run_with<F: TackleFacade>(mut app: App<F>, force_line: bool) -> ExitCode {
    let interactive = !force_line && std::io::stdout().is_terminal();
    let result = if interactive {
        run_fullscreen(&mut app)
    } else {
        let stdin = std::io::stdin();
        let mut reader = std::io::BufReader::new(stdin);
        let mut stdout = std::io::stdout();
        run_line_mode(&mut app, &mut reader, &mut stdout)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tackle-tui: {error}");
            ExitCode::FAILURE
        }
    }
}
