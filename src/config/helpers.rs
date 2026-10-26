use std::path::Path;

use crate::error::ConfigError;

pub const EXAMPLE: &str = include_str!("../../config.example.toml");

pub fn first_run_help(path: &Path) -> Option<String> {
    if path.is_dir() {
        return Some(format!(
            "ERROR: config path {} is a directory, not a file.\n\
             This happens when Docker bind-mounts a non-existent file - Docker creates a directory instead.\n\
             Fix:\n\
             1. Stop the container\n\
             2. Delete the directory: rm -rf {}\n\
             3. Create the config file on the host: touch {} && chmod 644 {}\n\
             4. Edit the file with your [[user]] and [[server]] entries (see config.example.toml)\n\
             5. Start the container again\n\
             \n\
             Alternatively, set JMP_CONFIG to point to a file inside a mounted directory volume instead of a single-file bind mount.",
            path.display(), path.display(), path.display(), path.display()
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

pub fn is_permission_error(e: &ConfigError) -> bool {
    matches!(e, ConfigError::Io { source, .. } if matches!(
        source.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    ))
}
