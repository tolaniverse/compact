//! Where Compact keeps its files, as both the app and `compact-mcp` see it.

use std::path::{Path, PathBuf};

/// The socket's file name inside the state directory.
pub const SOCKET_NAME: &str = "agent.sock";

/// `~/Library/Application Support/Compact`, or `COMPACT_STATE_DIR` when set,
/// so a test run or a second profile points somewhere else without touching
/// the real one. The app's state file lives here too.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("COMPACT_STATE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join("Library/Application Support/Compact"))
}

/// The longest socket path macOS accepts (`sun_path` is 104 bytes, one of
/// them the terminating NUL).
pub const MAX_SOCKET_PATH: usize = 103;

/// Why `path` cannot be a socket, if it cannot.
pub fn socket_path_problem(path: &Path) -> Option<String> {
    let n = path.as_os_str().len();
    (n > MAX_SOCKET_PATH).then(|| {
        format!(
            "the socket path is {n} bytes and macOS allows {MAX_SOCKET_PATH}; \
             point COMPACT_STATE_DIR somewhere shorter ({})",
            path.display()
        )
    })
}

/// Where the app listens for agents.
pub fn socket_path() -> Option<PathBuf> {
    state_dir().map(|d| d.join(SOCKET_NAME))
}

/// `path` with your home directory shortened to `~`, for showing to people.
pub fn tilde(path: &Path) -> String {
    let s = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && (s == home || s.starts_with(&format!("{home}/"))) => {
            format!("~{}", &s[home.len()..])
        }
        _ => s,
    }
}
