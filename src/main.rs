//! rustos-build CLI: test | build | image | run | clean

use rustos_build::{
    any_failed, cargo_build, cargo_clean, cargo_test, discover_repos, find_on_path,
    format_summary, run_command, RepoResult, StepStatus, REPO_ORDER,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage() -> &'static str {
    "rustos-build — RustOS orchestrator\n\n\
     USAGE:\n  \
     rustos-build test [--root <dir>]\n  \
     rustos-build build [--root <dir>] [--size-mb 64]\n  \
     rustos-build image [--root <dir>] [--kernel <elf>] [--size-mb 64]\n  \
     rustos-build run [--root <dir>] [--image <disk.img>] [--timeout 10]\n  \
     rustos-build clean [--root <dir>]\n\n\
     Default --root is the parent of this repo (the rust-os/ dir).\n"
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].clone())
}

fn default_root() -> PathBuf {
    // <root>/rustos-build/src/main.rs -> root is two levels up from CWD
    // when run from the repo; fall back to the parent of the current dir.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // If cwd is rustos-build itself, root is its parent.
    if cwd.file_name().map(|n| n == "rustos-build").unwrap_or(false) {
        cwd.parent().map(|p| p.to_path_buf()).unwrap_or(cwd)
    } else {
        cwd
    }
}

fn repo<'a>(repos: &'a [rustos_build::RepoInfo], name: &str) -> Option<&'a rustos_build::RepoInfo> {
    repos.iter().find(|r| r.name == name)
}

fn do_test(root: &Path) -> (Vec<RepoResult>, bool) {
    let repos = discover_repos(root);
    let mut results = Vec::new();
    for r in &repos {
        if r.present {
            println!("testing {} ...", r.name);
            results.push(cargo_test(r));
        } else {
            results.push(RepoResult::skipped(&r.name, &r.path));
        }
    }
    let failed = any_failed(&results);
    (results, failed)
}

/// Build a disk image via rustos-image (if present).
/// Returns a RepoResult named "image".
fn do_image(root: &Path, kernel_arg: Option<&str>, size_mb: u64) -> RepoResult {
    let repos = discover_repos(root);
    let Some(image_repo) = repo(&repos, "rustos-image") else {
        return RepoResult::skipped("image", &root.join("rustos-image"));
    };
    if !image_repo.present {
        return RepoResult::skipped("image", &image_repo.path);
    }
    // Resolve the kernel: explicit --kernel, else a built kernel binary,
    // else a generated placeholder kernel (demoable without a kernel repo).
    let kernel_path: PathBuf = if let Some(k) = kernel_arg {
        PathBuf::from(k)
    } else {
        let built = root
            .join("rustos-kernel/target/debug/rustos-kernel");
        if built.is_file() {
            built
        } else {
            let tmp = std::env::temp_dir().join(format!(
                "rustos-placeholder-kernel-{}.efi",
                std::process::id()
            ));
            // Minimal fake kernel: ELF magic + filler.
            let mut data = vec![0x7F, b'E', b'L', b'F', 2, 1, 1, 0];
            data.resize(4096, 0xAA);
            if std::fs::write(&tmp, &data).is_err() {
                return RepoResult::fail("image", "could not write placeholder kernel");
            }
            tmp
        }
    };
    if !kernel_path.is_file() {
        return RepoResult::fail(
            "image",
            &format!("kernel not found: {}", kernel_path.display()),
        );
    }
    let out = root.join("disk.img");
    let size = size_mb.to_string();
    let kernel_s = kernel_path.to_string_lossy().into_owned();
    let out_s = out.to_string_lossy().into_owned();
    match run_command(
        &image_repo.path,
        "cargo",
        &[
            "run",
            "--quiet",
            "--",
            "build",
            "--kernel",
            &kernel_s,
            "--out",
            &out_s,
            "--size-mb",
            &size,
        ],
    ) {
        Ok((true, _)) => RepoResult::pass(
            "image",
            &format!("built {} ({} MiB) from {}", out.display(), size_mb, kernel_path.display()),
        ),
        Ok((false, tail)) => RepoResult::fail("image", &format!("rustos-image failed: {}", tail.lines().last().unwrap_or("").trim())),
        Err(e) => RepoResult::fail("image", &e),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprint!("{}", usage());
        return ExitCode::from(2);
    }
    let rest = &args[2..];
    let root: PathBuf = flag(rest, "--root")
        .map(PathBuf::from)
        .unwrap_or_else(default_root);
    let size_mb: u64 = flag(rest, "--size-mb")
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    let sub = args[1].as_str();
    match sub {
        "test" => {
            let (results, failed) = do_test(&root);
            print!("{}", format_summary(&results));
            if failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        "build" => {
            let (mut results, failed) = do_test(&root);
            if !failed {
                let repos = discover_repos(&root);
                if let Some(kernel) = repo(&repos, "rustos-kernel") {
                    if kernel.present {
                        println!("building rustos-kernel ...");
                        results.push(cargo_build(kernel));
                    }
                }
                results.push(do_image(&root, None, size_mb));
            } else {
                results.push(RepoResult::skipped("image", &root.join("rustos-image")));
            }
            print!("{}", format_summary(&results));
            if any_failed(&results) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        "image" => {
            let kernel = flag(rest, "--kernel");
            let r = do_image(&root, kernel.as_deref(), size_mb);
            print!("{}", format_summary(&[r.clone()]));
            if r.status == StepStatus::Fail {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        "run" => {
            let image: PathBuf = flag(rest, "--image")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("disk.img"));
            let timeout: u64 = flag(rest, "--timeout")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10);
            let repos = discover_repos(&root);
            let runner = repo(&repos, "rustos-runner");
            if !image.is_file() {
                println!("run: SKIPPED — no image at {} (run `rustos-build image` first)", image.display());
                return ExitCode::SUCCESS;
            }
            if runner.map(|r| r.present).unwrap_or(false) {
                match find_on_path("qemu-system-x86_64") {
                    Some(p) => println!("QEMU found: {}", p),
                    None => {
                        println!("run: SKIPPED — QEMU (qemu-system-x86_64) is not installed.");
                        println!("  Debian/Ubuntu: sudo apt install qemu-system-x86");
                        println!("  Then re-run: rustos-build run --image {}", image.display());
                        return ExitCode::SUCCESS;
                    }
                }
                let img_s = image.to_string_lossy().into_owned();
                let to_s = timeout.to_string();
                let log = root.join("serial.log");
                let log_s = log.to_string_lossy().into_owned();
                match run_command(
                    &runner.unwrap().path,
                    "cargo",
                    &[
                        "run", "--quiet", "--", "run",
                        "--image", &img_s,
                        "--timeout", &to_s,
                        "--serial-log", &log_s,
                    ],
                ) {
                    Ok((ok, tail)) => {
                        println!("{}", tail.lines().last().unwrap_or(""));
                        if ok { ExitCode::SUCCESS } else { ExitCode::from(1) }
                    }
                    Err(e) => {
                        eprintln!("error: {}", e);
                        ExitCode::from(1)
                    }
                }
            } else {
                println!("run: SKIPPED — rustos-runner is missing at {}", root.join("rustos-runner").display());
                ExitCode::SUCCESS
            }
        }
        "clean" => {
            let repos = discover_repos(&root);
            let mut results = Vec::new();
            for r in &repos {
                results.push(cargo_clean(r));
            }
            print!("{}", format_summary(&results));
            if any_failed(&results) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        "-h" | "--help" | "help" => {
            print!("{}", usage());
            let _ = REPO_ORDER;
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: unknown subcommand '{}'", other);
            eprint!("{}", usage());
            ExitCode::from(2)
        }
    }
}
