//! Checks that no .rs file exceeds the 420-line limit

use std::path::Path;

#[test]
fn no_file_exceeds_420_lines() {
    let mut violations = Vec::new();

    // Check src/
    check_dir(Path::new("src"), &mut violations);

    // Check tests/
    check_dir(Path::new("tests"), &mut violations);

    if !violations.is_empty() {
        let mut msg = String::from("\nFiles exceeding 420 lines:\n");
        for (path, count) in &violations {
            msg.push_str(&format!("  {} ({} lines)\n", path, count));
        }
        msg.push_str("\nSplit large files into modules to stay under 420 lines per file.\n");
        panic!("{}", msg);
    }
}

fn check_dir(dir: &Path, violations: &mut Vec<(String, usize)>) {
    if !dir.exists() {
        return;
    }

    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();

        if path.is_dir() {
            check_dir(&path, violations);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let content = std::fs::read_to_string(&path).unwrap();
            let line_count = content.lines().count();

            if line_count > 420 {
                violations.push((path.display().to_string(), line_count));
            }
        }
    }
}
