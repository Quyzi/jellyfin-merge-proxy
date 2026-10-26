// Embeds the git commit (if available) as JMP_GIT_SHA for jmp_build_info.
fn main() {
    println!("cargo:rerun-if-env-changed=JMP_GIT_SHA");
    println!("cargo:rerun-if-changed=.git/HEAD");
    if std::path::Path::new(".git/refs/heads").exists() {
        println!("cargo:rerun-if-changed=.git/refs/heads");
    }
    let sha = std::env::var("JMP_GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        });
    println!(
        "cargo:rustc-env=JMP_GIT_SHA={}",
        sha.unwrap_or_else(|| "unknown".into())
    );
}
