//! First-run help when the config path is missing or a Docker bind-mount
//! directory trap.

use super::EXAMPLE;
use crate::error::ConfigError;
use std::path::Path;

/// First run (no config file yet): try to write `config.example.toml` next to
/// the configured `path` if its directory is writable. If not writable, return
/// instructions telling the user to create the file manually. `None` when the
/// config exists.
pub fn first_run_help(path: &Path) -> Option<String> {
    // Check if path is a directory (Docker creates a dir when bind-mounting a missing file)
    if path.is_dir() {
        return Some(format!(
            "ERROR: config path {} is a directory, not a file.\n\
             This happens when Docker bind-mounts a non-existent file - Docker creates a directory instead.\n\
             Fix:\n\
             1. Stop the container\n\
             2. On the HOST, delete the directory that was created at the host bind-mount path\n\
             3. On the HOST, create the config file: touch <host-path>/jmp.toml\n\
             4. Make it writable by the container user (uid 65532, or 99:100 on Unraid with --user): chown 65532:65532 <host-path>/jmp.toml\n\
             5. Edit the file with your [[user]] and [[server]] entries (see config.example.toml)\n\
             6. Start the container again\n\
             \n\
             Alternatively, set JMP_CONFIG to point to a file inside a mounted directory volume instead of a single-file bind mount.",
            path.display()
        ));
    }

    if path.exists() {
        return None;
    }
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let example_name = if path.file_name() == Some(std::ffi::OsStr::new("jmp.toml")) {
        "jmp.example.toml"
    } else {
        "config.example.toml"
    };
    let example = dir.join(example_name);

    let wrote = if example.exists() {
        Ok(false)
    } else {
        std::fs::create_dir_all(dir)
            .and_then(|_| std::fs::write(&example, EXAMPLE))
            .map(|_| true)
    };
    let mut msg = format!("no config file at {}.\n", path.display());
    match wrote {
        Ok(true) => msg += &format!("Wrote an annotated example to {}.\n", example.display()),
        Ok(false) => msg += &format!("An annotated example is at {}.\n", example.display()),
        Err(e) => {
            let (uid, gid) = (proc_status_id("Uid:"), proc_status_id("Gid:"));
            msg += &format!(
                "Could not write an example to {} ({e}). The config directory must be writable \
                 by this process (uid {uid}, gid {gid}); on the host run e.g. \
                 `chown -R {uid}:{gid} <the folder containing {}>`, or create the file from \
                 config.example.toml in the project repository.\n",
                example.display(),
                path.display()
            )
        }
    }
    msg += &format!(
        "Copy it to {} and edit it: [proxy], one [[user]] per person/TV and one [[server]] \
         per Jellyfin server. Plain HTTP is the default; TLS is optional. Then restart.",
        path.display()
    );
    Some(msg)
}

/// Effective uid/gid from /proc/self/status ("Uid:\t<real>\t<effective>...").
fn proc_status_id(key: &str) -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
        })
        .unwrap_or_else(|| "?".into())
}

pub(crate) fn is_permission_error(e: &ConfigError) -> bool {
    matches!(e, ConfigError::Io { source, .. } if matches!(
        source.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    ))
}
