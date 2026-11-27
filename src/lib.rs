//! rustos-build: the RustOS orchestrator (xtask).
//!
//! Discovers the sibling RustOS repos under a root directory, runs
//! `cargo test` in each in dependency order, builds the kernel and
//! a disk image, and prints a per-repo summary table. Missing repos
//! are SKIPPED, never a crash. Std-only.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Sibling repos, in dependency order.
pub const REPO_ORDER: &[&str] = &[
    "rustos-toolchain",
    "rustos-bootloader",
    // The framebuffer console (Batch 10): pixel drawing (rustos-fb)
    // plus the terminal layer (rustos-tty) the kernel mirrors its
    // console output through — before the kernel, which depends on
    // both.
    "rustos-fb",
    "rustos-tty",
    "rustos-kernel",
    "rustos-memory",
    "rustos-drivers",
    "rustos-interrupts",
    "rustos-scheduler",
    "rustos-process",
    "rustos-nic",
    "rustos-net",
    "rustos-netapps",
    "rustos-storage",
    "rustos-nvme",
    "rustos-usb",
    "rustos-fs",
    "rustos-image",
    "rustos-runner",
    "rustos-shell",
    "rustos-power",
    "rustos-users",
    "rustos-libc",
    // The first userspace program (Batch 8): depends on rustos-libc,
    // and the kernel's build.rs embeds its ELF — so it must be
    // built BEFORE the kernel in the `build` flow (see main.rs).
    "rustos-hello",
    // The userspace shell (Batch 9): same shape as rustos-hello —
    // depends on rustos-libc, embedded by the kernel's build.rs,
    // built BEFORE the kernel.
    "rustos-sh",
    "rustos-installer",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub name: String,
    pub path: PathBuf,
    /// True when the directory exists and contains a Cargo.toml.
    pub present: bool,
}

/// Discover all repos in `REPO_ORDER` under `root`, in that order.
/// A repo counts as present only if `<root>/<name>/Cargo.toml` exists.
pub fn discover_repos(root: &Path) -> Vec<RepoInfo> {
    REPO_ORDER
        .iter()
        .map(|name| {
            let path = root.join(name);
            let present = path.join("Cargo.toml").is_file();
            RepoInfo {
                name: name.to_string(),
                path,
                present,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Pass,
    Fail,
    Skipped,
}

impl StepStatus {
    pub fn label(&self) -> &'static str {
        match self {
            StepStatus::Pass => "PASS",
            StepStatus::Fail => "FAIL",
            StepStatus::Skipped => "SKIPPED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RepoResult {
    pub name: String,
    pub status: StepStatus,
    pub detail: String,
}

impl RepoResult {
    pub fn skipped(name: &str, path: &Path) -> Self {
        Self {
            name: name.to_string(),
            status: StepStatus::Skipped,
            detail: format!("missing: {}", path.display()),
        }
    }
    pub fn pass(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_string(),
            status: StepStatus::Pass,
            detail: detail.to_string(),
        }
    }
    pub fn fail(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_string(),
            status: StepStatus::Fail,
            detail: detail.to_string(),
        }
    }
}

/// Render the summary table. Pure function — unit-tested.
pub fn format_summary(results: &[RepoResult]) -> String {
    let mut out = String::new();
    out.push_str("RustOS build summary\n");
    let name_w = results
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(4)
        .max("repo".len());
    out.push_str(&format!(
        "{:<name_w$}  {:<8}  {}\n",
        "repo",
        "status",
        "detail",
        name_w = name_w
    ));
    out.push_str(&format!(
        "{:-<name_w$}  {:-<8}  {:-<6}\n",
        "",
        "",
        "",
        name_w = name_w
    ));
    for r in results {
        out.push_str(&format!(
            "{:<name_w$}  {:<8}  {}\n",
            r.name,
            r.status.label(),
            r.detail,
            name_w = name_w
        ));
    }
    let passed = results.iter().filter(|r| r.status == StepStatus::Pass).count();
    let failed = results.iter().filter(|r| r.status == StepStatus::Fail).count();
    let skipped = results
        .iter()
        .filter(|r| r.status == StepStatus::Skipped)
        .count();
    out.push_str(&format!(
        "total: {}  passed: {}  failed: {}  skipped: {}\n",
        results.len(),
        passed,
        failed,
        skipped
    ));
    out
}

pub fn any_failed(results: &[RepoResult]) -> bool {
    results.iter().any(|r| r.status == StepStatus::Fail)
}

/// Run a command in `dir`, capturing stdout+stderr.
/// Returns (success, combined output tail, up to ~2000 chars).
///
/// `RUSTUP_TOOLCHAIN` is stripped from the child environment: when this
/// orchestrator itself runs under `cargo run`, rustup sets that variable
/// to the *parent* toolchain (stable), and a child `cargo` would inherit
/// it instead of honouring the repo's own `rust-toolchain.toml`.
/// That broke the nightly repos (`rustos-bootloader`, `rustos-kernel` —
/// nightly is required for `-Zbuild-std` / `extern "x86-interrupt"`).
/// Stripping it lets rustup pick each repo's pinned toolchain.
pub fn run_command(dir: &Path, program: &str, args: &[&str]) -> Result<(bool, String), String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env_remove("RUSTUP_TOOLCHAIN")
        .output()
        .map_err(|e| format!("failed to spawn '{}': {}", program, e))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !stderr.is_empty() {
        combined.push_str(&stderr);
    }
    // Keep only the tail so a huge cargo log does not flood the summary.
    let tail: String = combined.chars().rev().take(2000).collect::<String>().chars().rev().collect();
    Ok((output.status.success(), tail))
}

/// Test args per repo. The kernel defaults to the bare-metal target
/// (`x86_64-unknown-none` in its `.cargo/config.toml`), which has no `std`
/// or test harness, so its host unit tests must name the host target
/// explicitly: `cargo test --target x86_64-unknown-linux-gnu`.
/// `rustos-hello` and `rustos-sh` have the same shape (a
/// `.cargo/config.toml` pinning `x86_64-unknown-none` for the program
/// binary, while their lib tests need `std`), so they get the same
/// special case. Every other repo tests with plain `cargo test`.
pub fn test_args_for(name: &str) -> Vec<&'static str> {
    match name {
        "rustos-kernel" | "rustos-hello" | "rustos-sh" => {
            vec!["test", "--quiet", "--target", "x86_64-unknown-linux-gnu"]
        }
        _ => vec!["test", "--quiet"],
    }
}

/// Run `cargo test` in a repo. Missing repos are SKIPPED.
pub fn cargo_test(repo: &RepoInfo) -> RepoResult {
    if !repo.present {
        return RepoResult::skipped(&repo.name, &repo.path);
    }
    match run_command(&repo.path, "cargo", &test_args_for(&repo.name)) {
        Ok((true, _)) => RepoResult::pass(&repo.name, "cargo test passed"),
        Ok((false, out)) => RepoResult::fail(
            &repo.name,
            &format!("cargo test failed: {}", last_line(&out)),
        ),
        Err(e) => RepoResult::fail(&repo.name, &e),
    }
}

/// Run `cargo build` in a repo. Missing repos are SKIPPED.
pub fn cargo_build(repo: &RepoInfo) -> RepoResult {
    if !repo.present {
        return RepoResult::skipped(&repo.name, &repo.path);
    }
    match run_command(&repo.path, "cargo", &["build"]) {
        Ok((true, _)) => RepoResult::pass(&repo.name, "cargo build passed"),
        Ok((false, out)) => RepoResult::fail(
            &repo.name,
            &format!("cargo build failed: {}", last_line(&out)),
        ),
        Err(e) => RepoResult::fail(&repo.name, &e),
    }
}

/// Run `cargo clean` in a repo. Missing repos are SKIPPED.
pub fn cargo_clean(repo: &RepoInfo) -> RepoResult {
    if !repo.present {
        return RepoResult::skipped(&repo.name, &repo.path);
    }
    match run_command(&repo.path, "cargo", &["clean"]) {
        Ok((true, _)) => RepoResult::pass(&repo.name, "cargo clean done"),
        Ok((false, out)) => RepoResult::fail(
            &repo.name,
            &format!("cargo clean failed: {}", last_line(&out)),
        ),
        Err(e) => RepoResult::fail(&repo.name, &e),
    }
}

/// Check `nm` output for the allocator-wiring invariant of a
/// userspace program binary: exactly one `RustosAlloc` static —
///
/// rustos-libc's `ALLOCATOR` — and no program-local `ALLOC`.
///
/// Why this exists (Batch 9 live boot, 2026-11-22): both user
/// programs exited 1 before printing anything. `start()` had
/// installed the heap into libc's `ALLOCATOR`, but each program
/// declared its own `#[global_allocator] static ALLOC` — a second,
/// never-initialised `RustosAlloc` — and `format!` allocates
/// through the global allocator, so the first print failed every
/// allocation, panicked, and exited 1. The fix is one allocator:
/// libc's `ALLOCATOR` carries `#[global_allocator]` on freestanding
/// targets and programs declare none. Host unit tests cannot see
/// that wiring (they build for the host target, where the
/// attribute is off), so this check reads the shipped binary's
/// symbol table instead: a program-local `ALLOC` static in the
/// freestanding ELF means the bug is back.
///
/// `nm` prints one symbol per line: `<address> <type> <name>`.
/// Only data/bss statics (types `b`/`B`/`d`/`D`/`r`/`R`) can be
/// allocator instances; function symbols (`t`/`T`, e.g.
/// `RustosAlloc::init`, `GlobalAlloc::alloc`) are ignored. The
/// check is pure over the output, so unit tests can drive it with
/// sample `nm` text.
pub fn check_allocator_symbols(nm_output: &str) -> Result<(), String> {
    let mut allocator = 0usize;
    let mut local_alloc = Vec::new();
    let mut other_alloc_statics = Vec::new();
    for line in nm_output.lines() {
        let mut parts = line.split_whitespace();
        let (_addr, ty, name) = match (parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(t), Some(n)) => (a, t, n),
            _ => continue,
        };
        if !matches!(ty, "b" | "B" | "d" | "D" | "r" | "R") {
            continue;
        }
        if !name.contains("ALLOC") {
            continue;
        }
        if name.contains("9ALLOCATOR") {
            allocator += 1;
        } else if name.ends_with("5ALLOC") {
            local_alloc.push(name.to_string());
        } else {
            other_alloc_statics.push(name.to_string());
        }
    }
    if allocator != 1 {
        return Err(format!(
            "expected exactly one rustos-libc ALLOCATOR static, found {}",
            allocator
        ));
    }
    if !local_alloc.is_empty() {
        return Err(format!(
            "program-local #[global_allocator] ALLOC static(s) present ({}): \
             start() initialises libc's ALLOCATOR, but format!/Vec/String \
             allocate through the program's own never-initialised ALLOC — \
             the Batch 9 exit(1) bug",
            local_alloc.join(", ")
        ));
    }
    if !other_alloc_statics.is_empty() {
        return Err(format!(
            "unexpected RustosAlloc static(s): {}",
            other_alloc_statics.join(", ")
        ));
    }
    Ok(())
}

/// Verify a userspace program binary's allocator wiring with `nm`:
/// the freestanding ELF at
/// `<repo>/target/x86_64-unknown-none/debug/<bin>` must contain
/// exactly one `RustosAlloc` static (libc's `ALLOCATOR`) — see
/// [`check_allocator_symbols`]. Missing repo/binary: SKIPPED.
/// `nm` not on PATH, or the check failing: FAIL.
pub fn verify_program_allocator(repo: &RepoInfo, bin: &str) -> RepoResult {
    let name = format!("{}-allocator", repo.name);
    if !repo.present {
        return RepoResult::skipped(&name, &repo.path);
    }
    let binary = repo
        .path
        .join("target")
        .join("x86_64-unknown-none")
        .join("debug")
        .join(bin);
    if !binary.is_file() {
        return RepoResult::skipped(&name, &binary);
    }
    let Some(nm) = find_on_path("nm") else {
        return RepoResult::fail(
            &name,
            "nm not found on PATH (binutils is required to verify the allocator wiring)",
        );
    };
    let output = std::process::Command::new(nm)
        .arg(&binary)
        .env_remove("RUSTUP_TOOLCHAIN")
        .output();
    let output = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        Ok(o) => {
            return RepoResult::fail(
                &name,
                &format!(
                    "nm failed on {}: {}",
                    binary.display(),
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
            );
        }
        Err(e) => return RepoResult::fail(&name, &format!("failed to spawn nm: {}", e)),
    };
    match check_allocator_symbols(&output) {
        Ok(()) => RepoResult::pass(
            &name,
            "one RustosAlloc static (libc ALLOCATOR), no program-local ALLOC",
        ),
        Err(e) => RepoResult::fail(&name, &e),
    }
}

fn last_line(out: &str) -> String {
    out.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("unknown error")
        .trim()
        .to_string()
}

/// Find a program on PATH (hand-rolled, std-only).
pub fn find_on_path(program: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(program);
        if cand.is_file() {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    fn tmp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("rustos-build-test-{}-{}", std::process::id(), n));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn discovery_finds_present_and_missing_in_order() {
        let root = tmp_root();
        // Two present repos, one directory without Cargo.toml, rest absent.
        fs::create_dir_all(root.join("rustos-image")).unwrap();
        fs::write(root.join("rustos-image/Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        fs::create_dir_all(root.join("rustos-shell")).unwrap();
        fs::write(root.join("rustos-shell/Cargo.toml"), "[package]\nname=\"y\"\n").unwrap();
        fs::create_dir_all(root.join("rustos-kernel")).unwrap(); // no Cargo.toml -> missing
        let repos = discover_repos(&root);
        assert_eq!(repos.len(), REPO_ORDER.len());
        assert_eq!(
            repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            REPO_ORDER
        );
        let by_name = |n: &str| repos.iter().find(|r| r.name == n).unwrap();
        assert!(by_name("rustos-image").present);
        assert!(by_name("rustos-shell").present);
        assert!(!by_name("rustos-kernel").present);
        assert!(!by_name("rustos-toolchain").present);
        assert_eq!(by_name("rustos-toolchain").path, root.join("rustos-toolchain"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn summary_formatting() {
        let results = vec![
            RepoResult::pass("rustos-image", "cargo test passed"),
            RepoResult::fail("rustos-kernel", "cargo test failed: boom"),
            RepoResult::skipped("rustos-fs", Path::new("/x/rustos-fs")),
        ];
        let s = format_summary(&results);
        assert!(s.contains("repo"));
        assert!(s.contains("rustos-image"));
        assert!(s.contains("PASS"));
        assert!(s.contains("FAIL"));
        assert!(s.contains("SKIPPED"));
        assert!(s.contains("missing: /x/rustos-fs"));
        assert!(s.contains("total: 3  passed: 1  failed: 1  skipped: 1"));
        assert!(any_failed(&results));
        assert!(!any_failed(&results[..1]));
    }

    #[test]
    fn summary_empty_and_all_skipped() {
        let s = format_summary(&[]);
        assert!(s.contains("total: 0"));
        let all_skip: Vec<RepoResult> = REPO_ORDER
            .iter()
            .map(|n| RepoResult::skipped(n, Path::new("/none")))
            .collect();
        assert!(!any_failed(&all_skip));
        // Derived, not hardcoded: Batch 9 added rustos-sh as the
        // 24th repo and the old literal "23" went stale (the suite
        // then failed on a count, not on behaviour).
        assert!(format_summary(&all_skip).contains(&format!("skipped: {}", REPO_ORDER.len())));
    }

    #[test]
    fn kernel_and_hello_tests_use_host_target() {
        assert_eq!(
            test_args_for("rustos-kernel"),
            vec!["test", "--quiet", "--target", "x86_64-unknown-linux-gnu"]
        );
        // rustos-hello pins the bare-metal target for its binary,
        // so its lib tests need the host target too (plain
        // `cargo test` there fails: no `std` on
        // x86_64-unknown-none).
        assert_eq!(
            test_args_for("rustos-hello"),
            vec!["test", "--quiet", "--target", "x86_64-unknown-linux-gnu"]
        );
        assert_eq!(test_args_for("rustos-fs"), vec!["test", "--quiet"]);
        assert_eq!(test_args_for("rustos-bootloader"), vec!["test", "--quiet"]);
    }

    #[test]
    fn run_command_strips_rustup_toolchain() {
        // A parent `cargo run` sets RUSTUP_TOOLCHAIN=stable; child cargo
        // invocations must NOT inherit it, or nightly repos break.
        std::env::set_var("RUSTUP_TOOLCHAIN", "stable-x86_64-unknown-linux-gnu");
        let dir = tmp_root();
        let (ok, out) = run_command(&dir, "sh", &["-c", "echo \"tc=[$RUSTUP_TOOLCHAIN]\""]).unwrap();
        assert!(ok);
        assert!(out.contains("tc=[]"), "child saw RUSTUP_TOOLCHAIN: {out}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_command_true_and_false() {
        let dir = tmp_root();
        let (ok, _) = run_command(&dir, "true", &[]).unwrap();
        assert!(ok);
        let (ok, _) = run_command(&dir, "false", &[]).unwrap();
        assert!(!ok);
        assert!(run_command(&dir, "definitely-not-a-real-program-xyz", &[]).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cargo_test_skips_missing_repo() {
        let repo = RepoInfo {
            name: "rustos-fs".to_string(),
            path: PathBuf::from("/nonexistent/rustos-fs"),
            present: false,
        };
        let r = cargo_test(&repo);
        assert_eq!(r.status, StepStatus::Skipped);
        assert!(r.detail.contains("missing"));
        assert_eq!(cargo_build(&repo).status, StepStatus::Skipped);
        assert_eq!(cargo_clean(&repo).status, StepStatus::Skipped);
    }

    /// Sample `nm` output for a FIXED userspace program binary:
    /// exactly one `RustosAlloc` static (libc's ALLOCATOR). The
    /// function symbols that mention `RustosAlloc`/`GlobalAlloc`
    /// must be ignored — only data/bss statics count.
    const NM_FIXED: &str = "\
0000000000400150 t _RNvCs5fGHjzWXRav_7___rustc12___rust_alloc
0000000000401e70 T _RNvMs0_NtCs7h9ePDcZgSm_11rustos_libc4heapNtB5_11RustosAlloc4init
0000000000402090 T _RNvXs2_NtCs7h9ePDcZgSm_11rustos_libc4heapNtB5_11RustosAllocNtNtNtCsbQJWu2zlgTK_4core5alloc6global11GlobalAlloc5alloc
0000000000407ec8 B _RNvNtCs7h9ePDcZgSm_11rustos_libc4heap9ALLOCATOR
0000000000400000 T _start
";

    /// The same, but as the Batch 9 live boot shipped it: the
    /// program's own never-initialised `ALLOC` beside libc's
    /// `ALLOCATOR` — `format!` allocated through the dead one and
    /// both programs exited 1 before printing anything.
    const NM_BROKEN: &str = "\
0000000000400150 t _RNvCs5fGHjzWXRav_7___rustc12___rust_alloc
0000000000401e70 T _RNvMs0_NtCs7h9ePDcZgSm_11rustos_libc4heapNtB5_11RustosAlloc4init
0000000000407ea0 b _RNvCskjHm0zGAuJv_5hello5ALLOC
0000000000407ec8 B _RNvNtCs7h9ePDcZgSm_11rustos_libc4heap9ALLOCATOR
0000000000400000 T _start
";

    #[test]
    fn allocator_wiring_accepts_the_single_allocator_binary() {
        assert_eq!(check_allocator_symbols(NM_FIXED), Ok(()));
    }

    #[test]
    fn allocator_wiring_rejects_a_program_local_alloc() {
        let err = check_allocator_symbols(NM_BROKEN).unwrap_err();
        assert!(err.contains("program-local"), "{err}");
        assert!(err.contains("5hello5ALLOC"), "{err}");
    }

    #[test]
    fn allocator_wiring_rejects_a_missing_allocator() {
        let none = "0000000000400000 T _start\n";
        let err = check_allocator_symbols(none).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        // Two ALLOCATOR statics is just as wrong as none: there
        // must be exactly one instance for start() to initialise.
        let two = format!("{NM_FIXED}0000000000407f00 B _RNvNtCs7h9ePDcZgSm_11rustos_libc4heap9ALLOCATOR\n");
        let err = check_allocator_symbols(&two).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
    }

    #[test]
    fn verify_allocator_skips_a_missing_binary() {
        let repo = RepoInfo {
            name: "rustos-hello".to_string(),
            path: PathBuf::from("/nonexistent/rustos-hello"),
            present: false,
        };
        assert_eq!(
            verify_program_allocator(&repo, "hello").status,
            StepStatus::Skipped
        );
        let repo = RepoInfo {
            name: "rustos-hello".to_string(),
            path: tmp_root(),
            present: true,
        };
        // Present repo, no freestanding binary built yet.
        assert_eq!(
            verify_program_allocator(&repo, "hello").status,
            StepStatus::Skipped
        );
        let _ = fs::remove_dir_all(&repo.path);
    }
}
