//! rustos-build CLI: test | build | image | run | clean

use rustos_build::{
    any_failed, cargo_build, cargo_clean, cargo_test, discover_repos, find_on_path,
    format_summary, run_command, verify_program_allocator, RepoResult, StepStatus, REPO_ORDER,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage() -> &'static str {
    "rustos-build — RustOS orchestrator\n\n\
     USAGE:\n  \
     rustos-build test [--root <dir>]\n  \
     rustos-build build [--root <dir>] [--size-mb 64]\n  \
     rustos-build image [--root <dir>] [--kernel <elf>] [--size-mb 64]\n  \
     rustos-build run [--root <dir>] [--image <disk.img>] [--timeout 10] \\\n      \
     [--ovmf] [--ovmf-code <path>] [--ovmf-vars <path>]\n  \
     rustos-build clean [--root <dir>]\n\n\
     Default --root is the parent of this repo (the rust-os/ dir).\n\n\
     `image`/`build` produce a BOOTABLE UEFI/GPT disk via\n  \
     rustos-bootloader: the ESP holds efi/boot/bootx64.efi, the kernel\n  \
     (kernel-x86_64) and boot.json. Without rustos-bootloader present\n  \
     they fall back to rustos-image, whose ESP holds only the kernel\n  \
     and is NOT UEFI-bootable. Boot the image with OVMF (see\n  \
     rustos-bootloader README for the exact QEMU command), or\n  \
     `rustos-build run --ovmf` to boot it under QEMU via rustos-run.\n"
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

/// Build a bootable disk image.
/// The bootable path uses `rustos-bootloader` (if present): its CLI
/// wraps the `bootloader` crate's `DiskImageBuilder`, which embeds
/// `efi/boot/bootx64.efi` + the kernel + `boot.json` on a GPT disk —
/// the image OVMF boots without intervention. Only when that repo is
/// missing do we fall back to `rustos-image`, whose ESP holds just
/// the kernel file and is NOT UEFI-bootable on its own (it is a
/// correctly partitioned container, not a bootable disk).
/// Returns a RepoResult named "image".
fn do_image(root: &Path, kernel_arg: Option<&str>, size_mb: u64) -> RepoResult {
    let repos = discover_repos(root);
    // Resolve the kernel: explicit --kernel, else a built kernel binary.
    // The kernel builds for x86_64-unknown-none (see its .cargo/config),
    // so the binary lives under target/x86_64-unknown-none/debug/;
    // the plain target/debug/ path is kept as a fallback.
    let kernel_path: Option<PathBuf> = if let Some(k) = kernel_arg {
        Some(PathBuf::from(k))
    } else {
        let triple = root.join("rustos-kernel/target/x86_64-unknown-none/debug/rustos-kernel");
        let plain = root.join("rustos-kernel/target/debug/rustos-kernel");
        if triple.is_file() {
            Some(triple)
        } else if plain.is_file() {
            Some(plain)
        } else {
            None
        }
    };
    let out = root.join("disk.img");
    let out_s = out.to_string_lossy().into_owned();
    if let Some(boot) = repo(&repos, "rustos-bootloader") {
        if boot.present {
            // The bootloader path validates the ELF, so a placeholder
            // fake kernel cannot be used here: a missing kernel is a
            // clear failure, not a silently unbootable image.
            let Some(kernel_path) = kernel_path else {
                return RepoResult::fail(
                    "image",
                    "kernel not found: build rustos-kernel first, or pass --kernel <elf>",
                );
            };
            let kernel_s = kernel_path.to_string_lossy().into_owned();
            return match run_command(
                &boot.path,
                "cargo",
                &["run", "--quiet", "--", "--kernel", &kernel_s, "--out", &out_s],
            ) {
                Ok((true, tail)) => RepoResult::pass(
                    "image",
                    &format!(
                        "bootable UEFI/GPT {} via rustos-bootloader ({}; --size-mb ignored: sized from content)",
                        out.display(),
                        tail.lines().next().unwrap_or("").trim()
                    ),
                ),
                Ok((false, tail)) => RepoResult::fail(
                    "image",
                    &format!(
                        "rustos-bootloader failed: {}",
                        tail.lines().last().unwrap_or("").trim()
                    ),
                ),
                Err(e) => RepoResult::fail("image", &e),
            };
        }
    }
    // Fallback: rustos-image only (kernel-only ESP, not UEFI-bootable).
    let image_repo = match repo(&repos, "rustos-image") {
        Some(r) if r.present => r,
        _ => return RepoResult::skipped("image", &root.join("rustos-image")),
    };
    let kernel_path: PathBuf = match kernel_path {
        Some(k) => k,
        None => {
            // No kernel repo: generated placeholder, demoable only.
            let tmp = std::env::temp_dir().join(format!(
                "rustos-placeholder-kernel-{}.efi",
                std::process::id()
            ));
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
    let size = size_mb.to_string();
    let kernel_s = kernel_path.to_string_lossy().into_owned();
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
            &format!(
                "built {} ({} MiB) from {} via rustos-image (kernel-only ESP: NOT UEFI-bootable, use rustos-bootloader)",
                out.display(),
                size_mb,
                kernel_path.display()
            ),
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
                // rustos-hello and rustos-sh FIRST: the kernel's
                // build.rs embeds both ELFs, so the kernel build
                // must find freshly built program binaries
                // (otherwise it embeds empty placeholders and boots
                // without userspace).
                if let Some(hello) = repo(&repos, "rustos-hello") {
                    if hello.present {
                        println!("building rustos-hello ...");
                        let r = cargo_build(hello);
                        let ok = r.status == StepStatus::Pass;
                        results.push(r);
                        if ok {
                            // The allocator wiring check (Batch 9
                            // exit(1) bug): the shipped binary must
                            // contain exactly one RustosAlloc static
                            // (libc's ALLOCATOR) — a program-local
                            // ALLOC would fail every allocation.
                            println!("verifying rustos-hello allocator wiring ...");
                            results.push(verify_program_allocator(hello, "hello"));
                        }
                    }
                }
                if let Some(sh) = repo(&repos, "rustos-sh") {
                    if sh.present {
                        println!("building rustos-sh ...");
                        let r = cargo_build(sh);
                        let ok = r.status == StepStatus::Pass;
                        results.push(r);
                        if ok {
                            println!("verifying rustos-sh allocator wiring ...");
                            results.push(verify_program_allocator(sh, "sh"));
                        }
                    }
                }
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
                let mut run_args: Vec<String> = vec![
                    "run".to_string(),
                    "--quiet".to_string(),
                    "--".to_string(),
                    "run".to_string(),
                    "--image".to_string(),
                    img_s,
                    "--timeout".to_string(),
                    to_s,
                    "--serial-log".to_string(),
                    log_s,
                ];
                // UEFI boot: forward --ovmf (and any explicit firmware
                // paths) to the runner; without it QEMU boots SeaBIOS.
                if rest.iter().any(|a| a == "--ovmf") {
                    run_args.push("--ovmf".to_string());
                }
                if let Some(c) = flag(rest, "--ovmf-code") {
                    run_args.push("--ovmf-code".to_string());
                    run_args.push(c);
                }
                if let Some(v) = flag(rest, "--ovmf-vars") {
                    run_args.push("--ovmf-vars".to_string());
                    run_args.push(v);
                }
                let run_refs: Vec<&str> =
                    run_args.iter().map(|s| s.as_str()).collect();
                match run_command(&runner.unwrap().path, "cargo", &run_refs) {
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
