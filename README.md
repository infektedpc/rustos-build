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
./target/debug/rustos-build run [--root ../] [--image disk.img] [--timeout 10] \
  [--ovmf] [--ovmf-code <path>] [--ovmf-vars <path>]
./target/debug/rustos-build clean [--root ../]
```

- `test` — `cargo test` in each present repo, in dependency order.
- `build` — `test`, then `cargo build` of the kernel (if present),
  then a bootable image build (see below).
- `image` — build a **bootable UEFI/GPT** disk image from a kernel
  ELF via `rustos-bootloader` (see "Boot flow"). The kernel is taken
  from `--kernel`, else `rustos-kernel/target/x86_64-unknown-none/
  debug/rustos-kernel`; a missing kernel is a failure, not a
  placeholder. Only when `rustos-bootloader` is absent does it fall
  back to `rustos-image` (kernel-only ESP, NOT UEFI-bootable).
- `run` — if an image exists and `rustos-runner` is present, boot it
  under QEMU and check the serial log for the marker (see
  rustos-runner README). `--ovmf` boots UEFI (q35 + OVMF pflash);
  without it QEMU boots SeaBIOS, which cannot boot this image.
  Booting needs QEMU installed; when it is missing `run` reports
  that clearly instead of failing obscurely.
- `clean` — `cargo clean` in each present repo.

## Boot flow (what OVMF executes)

`rustos-build image` (via `rustos-bootloader`, which wraps the
`bootloader` 0.11 crate's `DiskImageBuilder`) writes a GPT disk
whose EFI System Partition holds exactly three files:

| ESP path | Contents |
|---|---|
| `efi/boot/bootx64.efi` | The bootloader crate's embedded UEFI bootloader — what OVMF executes first (the default UEFI boot entry). |
| `kernel-x86_64` | The RustOS kernel ELF (`rustos-kernel`), loaded by the bootloader. |
| `boot.json` | The serialised default `BootConfig` (the bootloader uses the same default when the file is absent). |

OVMF reads the GPT, finds the ESP, runs `efi/boot/bootx64.efi`,
which loads `kernel-x86_64` and jumps to its entry point
(`bootloader_api` 0.11 `entry_point!`, receiving `BootInfo`).

Boot it by hand (copy the VARS file first — QEMU writes to it):

```sh
cp /usr/share/OVMF/OVMF_VARS_4M.fd /tmp/OVMF_VARS.fd
qemu-system-x86_64 -machine q35,accel=tcg -m 512M \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file=/tmp/OVMF_VARS.fd \
  -drive format=raw,file=disk.img -serial stdio -display none
```

The kernel prints to VGA and to serial COM1, so with `-serial stdio`
you see it boot in the terminal. Or let the orchestrator do it:
`rustos-build run --ovmf` (same QEMU invocation, serial log to
`serial.log`, marker check via `rustos-runner`).

Exit code: `0` when every present repo passed (skipped repos do not
fail the run), `1` when any present repo failed.

## Dependency order

`rustos-toolchain`, `rustos-bootloader`, `rustos-kernel`,
`rustos-memory`, `rustos-drivers`, `rustos-fs`, `rustos-image`,
`rustos-runner`, `rustos-shell`.

## Limitations

- `image` shells out to the `rustos-bootloader` CLI via `cargo run`
  when that repo is present (fallback: the `rustos-image` CLI); it
  does not (yet) link either image builder as a library.
- `--size-mb` is ignored on the bootloader path: the `bootloader`
  crate sizes the image from its content (kernel + bootloader).
- `run` does not boot without QEMU installed; it reports that
  clearly instead of failing obscurely.
