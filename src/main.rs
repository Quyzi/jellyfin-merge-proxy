fn main() {
    println!(
        "jellyfin-merge-proxy {} ({})",
        env!("CARGO_PKG_VERSION"),
        jmp::telemetry::git_sha()
    );
}
