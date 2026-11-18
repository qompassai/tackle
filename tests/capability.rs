//! Capability file and environment config.

use std::os::unix::fs::PermissionsExt;

use tackle::capability::{self, CapabilityError, Token};
use tackle::config::{self, ConfigError, Environment, PORT_DEFAULT};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn ok_publish_creates_private_dir_and_file() {
    let state = tempfile::tempdir().expect("temp");
    let state_dir = state.path().join("nested/tackle");
    let token = Token::generate().expect("token");
    let file = capability::publish(&state_dir, 9770, &token).expect("publish");
    assert_eq!(file, state_dir.join("capability/editor.json"));
    assert_eq!(mode(&state_dir.join("capability")), 0o700);
    assert_eq!(mode(&file), 0o600);
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("json");
    assert_eq!(body["version"], 1);
    assert_eq!(body["port"], 9770);
    assert_eq!(body["token"], token.expose());
}

#[test]
fn ok_republish_overwrites_with_new_token() {
    let state = tempfile::tempdir().expect("temp");
    let first = Token::generate().expect("token");
    let second = Token::generate().expect("token");
    assert_ne!(first, second);
    capability::publish(state.path(), 9770, &first).expect("first");
    let file = capability::publish(state.path(), 9771, &second).expect("second");
    let text = std::fs::read_to_string(file).expect("read");
    assert!(text.contains(second.expose()) && !text.contains(first.expose()));
}

#[test]
fn ok_token_is_256_bit_hex_and_redacted_in_debug() {
    let token = Token::generate().expect("token");
    assert_eq!(token.expose().len(), 64);
    assert!(token.expose().bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(token.matches(token.expose()));
    let debug = format!("{token:?}");
    assert!(!debug.contains(token.expose()), "{debug}");
}

#[test]
fn ok_config_defaults_and_precedence() {
    let home_only = Environment {
        home: Some("/home/someone"),
        ..Environment::default()
    };
    let config = config::resolve(home_only).expect("config");
    assert_eq!(config.port, PORT_DEFAULT);
    assert_eq!(
        config.state_dir.to_str(),
        Some("/home/someone/.local/state/tackle")
    );
    let xdg = Environment {
        xdg_state_home: Some("/x"),
        ..home_only
    };
    assert_eq!(
        config::resolve(xdg).expect("xdg").state_dir.to_str(),
        Some("/x/tackle")
    );
    let explicit = Environment {
        port: Some("9800"),
        state_dir: Some("/s"),
        ..xdg
    };
    let config = config::resolve(explicit).expect("explicit");
    assert_eq!((config.port, config.state_dir.to_str()), (9800, Some("/s")));
}

#[test]
fn adv_loose_capability_dir_refused_not_repaired() {
    let state = tempfile::tempdir().expect("temp");
    let dir = state.path().join("capability");
    std::fs::create_dir(&dir).expect("mkdir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let token = Token::generate().expect("token");
    let error = capability::publish(state.path(), 9770, &token).expect_err("refused");
    assert!(matches!(error, CapabilityError::Unsafe { .. }), "{error}");
    assert_eq!(mode(&dir), 0o755, "never silently repaired");
    assert!(!dir.join("editor.json").exists());
}

#[test]
fn adv_symlinked_capability_dir_refused() {
    let state = tempfile::tempdir().expect("temp");
    let elsewhere = tempfile::tempdir().expect("temp");
    std::fs::set_permissions(elsewhere.path(), std::fs::Permissions::from_mode(0o700))
        .expect("chmod");
    std::os::unix::fs::symlink(elsewhere.path(), state.path().join("capability")).expect("ln");
    let token = Token::generate().expect("token");
    let error = capability::publish(state.path(), 9770, &token).expect_err("refused");
    assert!(error.to_string().contains("symlink"), "{error}");
    assert!(!elsewhere.path().join("editor.json").exists());
}

#[test]
fn adv_token_mismatch_lengths_and_bytes() {
    let token = Token::from_string("ab".repeat(32));
    assert!(!token.matches(""));
    assert!(!token.matches(&"ab".repeat(31)));
    assert!(!token.matches(&format!("{}ac", "ab".repeat(31))));
    assert!(!token.matches(&"AB".repeat(32)));
}

#[test]
fn adv_bad_port_and_relative_state_dir_refused() {
    let base = Environment {
        home: Some("/h"),
        ..Environment::default()
    };
    for port in ["80", "0", "65536", "-1", "9770x"] {
        let error = config::resolve(Environment {
            port: Some(port),
            ..base
        })
        .expect_err(port);
        assert!(matches!(
            error,
            ConfigError::Invalid {
                variable: "TACKLE_PORT",
                ..
            }
        ));
    }
    let relative = Environment {
        state_dir: Some("relative/dir"),
        ..base
    };
    assert!(config::resolve(relative).is_err());
    assert_eq!(
        config::resolve(Environment::default()),
        Err(ConfigError::NoStateDir)
    );
}
