//! Per-run capability token and its private file (simplified godot-ai shape).
//!
//! Contract: each server run generates a fresh 256-bit token from the OS
//! CSPRNG and publishes `{"version":1,"port":P,"token":T}` to
//! `<state_dir>/capability/editor.json`. The directory must be a real
//! directory (not a symlink) with mode 0700; the file is written 0600 via
//! temp-file + rename. A directory that exists with looser permissions is a
//! startup error, never silently repaired. Rotation and ancestor-permission
//! checks are U1 work.

use std::fmt;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Token entropy, in bytes (hex-encoded on the wire: twice this many chars).
pub const TOKEN_BYTES: usize = 32;
pub const CAPABILITY_DIR_NAME: &str = "capability";
pub const CAPABILITY_FILE_NAME: &str = "editor.json";
pub const CAPABILITY_FILE_VERSION: u32 = 1;
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error("capability {path}: {cause}")]
    Io { path: String, cause: String },
    #[error("capability {path}: refusing: {reason}")]
    Unsafe { path: String, reason: String },
}

fn io_error(path: &Path) -> impl Fn(std::io::Error) -> CapabilityError + '_ {
    move |error| CapabilityError::Io {
        path: path.display().to_string(),
        cause: error.to_string(),
    }
}

/// A capability token. `Debug` and `Display` redact it so it cannot reach
/// logs by accident; [`Token::expose`] is the only way to read it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Read [`TOKEN_BYTES`] from the OS CSPRNG.
    pub fn generate() -> std::io::Result<Token> {
        let mut bytes = [0u8; TOKEN_BYTES];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex.len(), TOKEN_BYTES * 2);
        Ok(Token(hex))
    }

    /// Wrap a known value (tests and fixtures).
    pub fn from_string(value: String) -> Token {
        Token(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Length-checked, constant-time over equal lengths.
    pub fn matches(&self, presented: &str) -> bool {
        let expected = self.0.as_bytes();
        let presented = presented.as_bytes();
        if expected.len() != presented.len() {
            return false;
        }
        expected
            .iter()
            .zip(presented)
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Token(<redacted>)")
    }
}

/// Create (or validate) the private directory and publish the token file.
/// Returns the published file's path.
pub fn publish(state_dir: &Path, port: u16, token: &Token) -> Result<PathBuf, CapabilityError> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(state_dir)
        .map_err(io_error(state_dir))?;
    let dir = state_dir.join(CAPABILITY_DIR_NAME);
    match DirBuilder::new().mode(DIR_MODE).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io_error(&dir)(error)),
    }
    let dir_uid = validate_private_dir(&dir)?;
    let file = dir.join(CAPABILITY_FILE_NAME);
    let temp = dir.join(format!("{CAPABILITY_FILE_NAME}.tmp-{}", std::process::id()));
    let body = serde_json::json!({
        "version": CAPABILITY_FILE_VERSION,
        "port": port,
        "token": token.expose(),
    })
    .to_string();
    let mut handle = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(&temp)
        .map_err(io_error(&temp))?;
    handle.write_all(body.as_bytes()).map_err(io_error(&temp))?;
    handle.sync_all().map_err(io_error(&temp))?;
    drop(handle);
    std::fs::rename(&temp, &file).map_err(io_error(&file))?;
    let metadata = std::fs::symlink_metadata(&file).map_err(io_error(&file))?;
    // We just created it: anything else means a race with another writer.
    if metadata.permissions().mode() & 0o777 != FILE_MODE || metadata.uid() != dir_uid {
        return Err(unsafe_error(
            &file,
            "published file mode or owner changed underneath us",
        ));
    }
    Ok(file)
}

/// Refuse symlinks, non-directories and any group/world permission bit.
/// Returns the directory owner's uid.
fn validate_private_dir(dir: &Path) -> Result<u32, CapabilityError> {
    let metadata = std::fs::symlink_metadata(dir).map_err(io_error(dir))?;
    if metadata.file_type().is_symlink() {
        return Err(unsafe_error(dir, "is a symlink"));
    }
    if !metadata.is_dir() {
        return Err(unsafe_error(dir, "is not a directory"));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(unsafe_error(
            dir,
            &format!("mode {mode:o} grants group/world access; expected {DIR_MODE:o}"),
        ));
    }
    Ok(metadata.uid())
}

fn unsafe_error(path: &Path, reason: &str) -> CapabilityError {
    CapabilityError::Unsafe {
        path: path.display().to_string(),
        reason: reason.to_string(),
    }
}
