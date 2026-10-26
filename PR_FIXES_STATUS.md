# Critical PR Fixes - Status Report

## ✅ Completed Fixes

### PR #2 (cursor/secret-encryption-path-migration-4c74) - ALL DONE
Branch pushed with 3 commits:

1. **`fix(config): improve secret rewriting robustness and documentation` (bde8c96)**
   - ✅ Fix #1: `write_atomic` falls back on EACCES/EPERM/EROFS (not just EXDEV/EBUSY)
   - ✅ Fix #2: Initialize logging early so Config::load warnings are visible
   - ✅ Fix #3: Preserve TOML inline comments (clone/restore decor())
   - ✅ Fix #4: Dockerfile uses build stage for directory creation (no shell required)

2. **`docs: fix container path instructions in README and compose` (a134009)**
   - ✅ Fix #5: Updated first_run_help, README, docker-compose.yml
   - Clarified HOST paths vs container paths
   - Changed from chmod 644 to chown 65532:65532
   - Added note about Unraid (99:100 with --user)

**All tests pass:** `cargo test --lib config_rewrite` ✅

### PR #3 (cursor/release-automation-4c74) - PARTIAL
Branch pushed with 1 commit:

1. **`fix(build): remove cargo-deb/cargo-generate-rpm build-dependencies` (008b056)**
   - ✅ Fix #6: Removed optional [build-dependencies]
   - Reduces Cargo.lock by ~890 lines
   - Removes source of RUSTSEC-2026-* zstd vulnerabilities
   - Note: Other zstd vulns remain (likely reqwest dependencies)

## ⏳ Remaining Fixes for PR #3

These require additional implementation:

### Fix #7: Ship minimal config in packages
**Files:** Cargo.toml ([package.metadata.deb]), config.example.toml
**Work needed:**
- Create minimal /etc/jmp.toml with no [[user]] or [[server]] entries
- Move full example to /usr/share/doc/jellyfin-merge-proxy/config.example.toml
- Ensure proxy starts cleanly or exits with clear message on minimal config
- Apply same pattern to rpm metadata
- Test: package, install, start service

### Fix #8: Add systemd-units metadata
**File:** Cargo.toml
**Work needed:**
```toml
[package.metadata.deb.systemd-units]
unit-name = "jellyfin-merge-proxy"
enable = false
restart-after-upgrade = true
stop-on-upgrade = false
```
- Ensures #DEBHELPER# is replaced
- Service restarts on upgrade
- Test: install, upgrade, verify service behavior

### Fix #9: Fix rpm file mode to 640
**File:** Cargo.toml ([package.metadata.generate-rpm.assets])
**Work needed:**
```toml
{ source = "config.example.toml", dest = "/etc/jmp.toml", mode = "640", config = "noreplace", user = "jmp", group = "jmp" }
```
- Prevents `rpm -V` from flagging `SM5..UGT`
- Test: build rpm, install, verify with `rpm -V`

### Fix #10: Re-enable systemd hardening
**File:** jellyfin-merge-proxy.service
**Work needed:**
- Re-enable MemoryDenyWriteExecute=true
- Re-enable SystemCallFilter=~@privileged (or add specific syscalls)
- Add UMask=0077
- Fix/remove misleading @privileged/mlock comment
- Consider RemoveIPC=true and PrivateUsers
- Test: start service under systemd, verify argon2 logins work

## Additional Testing Needed

### PR #2:
- Add test for errno fallback paths (EACCES, EROFS, EPERM)
- Add test for comment preservation

### PR #3:
- cargo audit (note remaining zstd vulns from other deps)
- docker build (verify with buildah if docker unavailable)
- Test .deb: install, start, verify service behavior
- Test .rpm: install, start, `rpm -V` output
- Full GitHub CI (fmt, clippy, test, audit, docker)

## Branch Management

After PR #2/#3 fixes are complete and CI is green:

1. Rebase `cursor/refactor-large-files-4c74` onto latest PR #3
   - Currently based on old PR #3 commit
   - Has 6 commits for module splitting

2. Rebase `cursor/sqlite-persistence-4c74` onto refactor branch
   - Currently has 3 commits (dependency, schema, docs)
   - Substantial work remains (~1,500-2,000 LOC)

## Next Steps

1. ✅ Push PR #2 fixes (DONE)
2. ✅ Push PR #3 cargo-deb/rpm removal (DONE)
3. ⏳ Implement remaining PR #3 fixes (#7-#10)
4. ⏳ Add tests for both PRs
5. ⏳ Wait for GitHub CI to be fully green
6. ⏳ Rebase module-split and SQLite PRs
7. ⏳ Report actual CI results with run links

## Files Modified

### PR #2:
- src/config_rewrite.rs - fallback logic, comment preservation
- src/main.rs - logging initialization order
- src/config.rs - first_run_help documentation
- Dockerfile - directory creation fix
- README.md - container instructions
- docker-compose.yml - container instructions

### PR #3:
- Cargo.toml - removed [build-dependencies]
- Cargo.lock - regenerated (reduced size)

### Pending PR #3:
- Cargo.toml - systemd-units, rpm mode
- jellyfin-merge-proxy.service - hardening options
- config.example.toml (or new minimal.toml)
- (tests)

## SQLite Addition

New requirement documented in `SQLITE_MIGRATION_PLAN.md` (on sqlite branch):
- Add `[database]` config section
- JournalMode enum (wal/delete/truncate/persist/memory/off)
- Synchronous enum (off/normal/full/extra)
- Read back and warn on mismatch
- Export jmp_db_journal_mode gauge
- Document for Unraid (FUSE/user share → delete mode)
