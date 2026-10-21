//! Fail-closed environment configuration.
//!
//! Contract: `$TACKLE_PORT` (default 9770) must be an unprivileged port;
//! the state directory is `$TACKLE_STATE_DIR`, else `$XDG_STATE_HOME/tackle`,
//! else `$HOME/.local/state/tackle`, and must be absolute. Anything else is
//! a startup error naming the variable.

use std::path::PathBuf;

/// Editor WebSocket port when `$TACKLE_PORT` is unset.
pub const PORT_DEFAULT: u16 = 9770;
/// Lowest accepted port; privileged ports are refused.
pub const PORT_MIN: u16 = 1024;
pub const PORT_ENV: &str = "TACKLE_PORT";
pub const STATE_DIR_ENV: &str = "TACKLE_STATE_DIR";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("{variable}: {reason}")]
    Invalid {
        variable: &'static str,
        reason: String,
    },
    #[error("no state directory: set {STATE_DIR_ENV}, XDG_STATE_HOME or HOME")]
    NoStateDir,
}

/// A validated configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub port: u16,
    /// Absolute; holds the private capability directory.
    pub state_dir: PathBuf,
}

/// Raw environment values, passed in so tests need not mutate the process
/// environment.
#[derive(Debug, Default, Clone, Copy)]
pub struct Environment<'a> {
    pub port: Option<&'a str>,
    pub state_dir: Option<&'a str>,
    pub xdg_state_home: Option<&'a str>,
    pub home: Option<&'a str>,
}

pub fn resolve(environment: Environment<'_>) -> Result<Config, ConfigError> {
    let port = match environment.port.filter(|value| !value.is_empty()) {
        None => PORT_DEFAULT,
        Some(text) => parse_port(text)?,
    };
    let non_empty = |value: Option<&str>| value.filter(|text| !text.is_empty()).map(PathBuf::from);
    let state_dir = if let Some(explicit) = non_empty(environment.state_dir) {
        explicit
    } else if let Some(xdg) = non_empty(environment.xdg_state_home) {
        xdg.join("tackle")
    } else if let Some(home) = non_empty(environment.home) {
        home.join(".local/state/tackle")
    } else {
        return Err(ConfigError::NoStateDir);
    };
    if !state_dir.is_absolute() {
        return Err(ConfigError::Invalid {
            variable: STATE_DIR_ENV,
            reason: format!("`{}` is not an absolute path", state_dir.display()),
        });
    }
    Ok(Config { port, state_dir })
}

fn parse_port(text: &str) -> Result<u16, ConfigError> {
    let invalid = |reason: String| ConfigError::Invalid {
        variable: PORT_ENV,
        reason,
    };
    let port: u16 = text
        .parse()
        .map_err(|_| invalid(format!("`{text}` is not a port number")))?;
    if port < PORT_MIN {
        return Err(invalid(format!("{port} is below {PORT_MIN}")));
    }
    Ok(port)
}
