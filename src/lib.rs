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
/// Every other repo tests with plain `cargo test`.
pub fn test_args_for(name: &str) -> Vec<&'static str> {
    match name {
        "rustos-kernel" => vec!["test", "--quiet", "--target", "x86_64-unknown-linux-gnu"],
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
        assert!(format_summary(&all_skip).contains("skipped: 18"));
    }

    #[test]
    fn kernel_test_uses_host_target() {
        assert_eq!(
            test_args_for("rustos-kernel"),
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
}
