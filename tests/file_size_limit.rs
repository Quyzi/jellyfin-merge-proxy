//! Guard: every Rust source file in the repository stays at or below
//! [`MAX_LINES`] lines. Split a file into modules rather than raising this.

use std::path::{Path, PathBuf};

const MAX_LINES: usize = 420;

/// Every `.rs` file under `dir`, skipping build output and hidden directories.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_rust_file_is_at_most_420_lines() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(root, &mut files);
    assert!(
        files.iter().any(|f| f.ends_with("src/lib.rs")),
        "walked the wrong directory: {}",
        root.display()
    );
    let mut too_long: Vec<String> = files
        .iter()
        .filter_map(|f| {
            let n = std::fs::read_to_string(f).unwrap().lines().count();
            let rel = f.strip_prefix(root).unwrap_or(f).display();
            (n > MAX_LINES).then(|| format!("{rel}: {n} lines"))
        })
        .collect();
    too_long.sort();
    assert!(
        too_long.is_empty(),
        "files over {MAX_LINES} lines:\n{}",
        too_long.join("\n")
    );
}
