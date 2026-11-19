//! Loading and saving the daemon's config file.

use std::{
    env, fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    process::Command,
};
use touchbar_daemon::config::{self, Config};

/// The config the daemon reads; `TOUCHBAR_CONFIG` overrides it for testing.
pub fn path() -> PathBuf {
    env::var_os("TOUCHBAR_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config::DEFAULT_PATH.into())
}

/// The result of loading: the config, or the defaults plus the reason the
/// file could not be used.
pub fn load(path: &Path) -> (Config, Option<String>) {
    match Config::load(path) {
        Ok(config) => (config, None),
        Err(e) => (
            Config::parse(config::DEFAULT_CONFIG).expect("built-in config is valid"),
            Some(format!("{e:#}")),
        ),
    }
}

#[derive(Debug)]
pub enum SaveError {
    /// The app is not allowed to write the file yet.
    NeedsPermission,
    Other(String),
}

/// Writes the config, replacing the file atomically when the directory
/// allows it and in place otherwise.
pub fn save(path: &Path, config: &Config) -> Result<(), SaveError> {
    let text = config.to_toml();
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".config.toml.{}.tmp", std::process::id()));
    let atomic = fs::write(&tmp, &text).and_then(|_| fs::rename(&tmp, path));
    let result = match atomic {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            if e.kind() == ErrorKind::PermissionDenied && path.exists() {
                fs::write(path, &text)
            } else {
                Err(e)
            }
        }
    };
    result.map_err(|e| match e.kind() {
        ErrorKind::PermissionDenied | ErrorKind::NotFound => SaveError::NeedsPermission,
        _ => SaveError::Other(e.to_string()),
    })
}

/// Lets the current user edit the config without a password from now on,
/// by giving their group write access to the config directory. Asks for
/// the administrator password once, through polkit.
pub fn grant_permission(path: &Path) -> io::Result<bool> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let gid = unsafe { libc_getgid() };
    let script = r#"set -e
dir="$1"; file="$2"; gid="$3"
mkdir -p "$dir"
chgrp "$gid" "$dir"
chmod 2775 "$dir"
if [ -f "$file" ]; then chgrp "$gid" "$file"; chmod 664 "$file"; fi
"#;
    let status = Command::new("pkexec")
        .args(["sh", "-c", script, "oma-touchbar"])
        .arg(dir)
        .arg(path)
        .arg(gid.to_string())
        .status()?;
    Ok(status.success())
}

unsafe extern "C" {
    #[link_name = "getgid"]
    fn libc_getgid() -> u32;
}
