# rustos-build

The RustOS build orchestrator (the `xtask`). It discovers the sibling
RustOS repos next to it, runs their test suites in dependency order,
then builds the kernel and a disk image, and prints a per-repo
pass/fail summary table.

Expected layout (all repos are siblings under one parent directory):

```
rust-os/
  rustos-toolchain/  rustos-bootloader/  rustos-kernel/
  rustos-memory/     rustos-drivers/     rustos-fs/
  rustos-image/      rustos-runner/      rustos-shell/
  rustos-build/      <- this repo
```

Repos that do not exist yet are reported as **SKIPPED** (with their
expected path) — never a crash. Only repos actually present are
tested/built.

## Build / test

```sh
cargo test
cargo build --release
```

## Usage

Run from this repo (it looks in `..` by default), or pass `--root`:

```sh
./target/debug/rustos-build test [--root ../]
./target/debug/rustos-build build [--root ../] [--size-mb 64]
./target/debug/rustos-build image [--root ../] [--kernel <elf>] [--size-mb 64]
./target/debug/rustos-build run [--root ../] [--image disk.img] [--timeout 10]
./target/debug/rustos-build clean [--root ../]
```

- `test` — `cargo test` in each present repo, in dependency order.
- `build` — `test`, then `cargo build` of the kernel (if present),
  then an image build (if `rustos-image` is present).
- `image` — build a disk image from a kernel ELF (default: a
  placeholder kernel is generated in a temp file when no `--kernel`
  is given and no kernel binary exists, so the step is demoable).
- `run` — if an image exists and `rustos-runner` is present, report
  QEMU status via `rustos-run check`; actually booting needs QEMU
  (see rustos-runner README for install instructions) and is not
  attempted when QEMU is missing.
- `clean` — `cargo clean` in each present repo.

Exit code: `0` when every present repo passed (skipped repos do not
fail the run), `1` when any present repo failed.

## Dependency order

`rustos-toolchain`, `rustos-bootloader`, `rustos-kernel`,
`rustos-memory`, `rustos-drivers`, `rustos-fs`, `rustos-image`,
`rustos-runner`, `rustos-shell`.

## Limitations

- `image` uses the `rustos-image` CLI via `cargo run` when that
  repo is present; it does not (yet) link the image builder as a
  library.
- `run` does not boot without QEMU installed; it reports that
  clearly instead of failing obscurely.
