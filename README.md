# krun-build-image

A Rust CLI that builds OCI images from a Containerfile inside a lightweight Linux microVM — using [libkrun](https://github.com/libkrun/libkrun).

## What it does

When you run `krun-build-image`, it:

1. Spins up a lightweight Linux microVM on your Mac using Apple's [Hypervisor.framework](https://developer.apple.com/documentation/hypervisor) — no Docker, no QEMU, no root required.
2. Mounts three host directories into the VM via [virtio-fs](https://virtio-fs.gitlab.io/): the build rootfs (as `/`), the build context, and the output directory.
3. Runs `buildah bud` inside the VM to build the image from the Containerfile.
4. Exports the resulting image as a Docker archive (`.tar`) to the host and exits.

The VM is fully isolated: it runs its own Linux kernel with its own process namespace, but shares no persistent state with your host.

## Usage

```sh
krun-build-image [OPTIONS] <CONTEXT>
```

| Argument / Option | Description | Default |
|---|---|---|
| `<CONTEXT>` | Build context directory | (required) |
| `--rootfs <DIR>` | VM root filesystem — must have `buildah` installed | `<binary dir>/rootfs` |
| `-f, --file <FILE>` | Path to the Containerfile | `<CONTEXT>/Containerfile` |
| `-o, --output <FILE>` | Output path for the Docker archive | `./output.tar` |
| `-t, --tag <TAG>` | Image tag used by buildah | `krun-build` |
| `--cpus <N>` | Number of vCPUs | `2` |
| `--memory <MB>` | RAM in MiB | `4096` |

Example:

```sh
krun-build-image -t myapp:latest ./myproject
```

This builds the image from `./myproject/Containerfile`, uses the rootfs bundled alongside the binary, and writes a Docker archive to `./output.tar`. Load it into Podman with:

```sh
podman load -i output.tar
```

If you have a rootfs elsewhere, or the Containerfile is outside the context directory:

```sh
krun-build-image --rootfs /tmp/build-rootfs -f ../Containerfile -t myapp:latest ./myproject
```

## How it uses libkrun

[libkrun](https://github.com/libkrun/libkrun) is a library that turns the virtual machine setup dance — kernel, memory, vCPUs, virtio devices — into a handful of function calls. Under the hood it uses Apple's Hypervisor.framework on macOS, so it requires no kernel extensions and no elevated privileges.

The Rust crate [`krun-sys`](https://crates.io/crates/krun-sys) provides generated FFI bindings to libkrun's C API. This app calls the following functions:

| Call | What it does |
|------|-------------|
| `krun_create_ctx()` | Allocates a new VM context; returns an integer context ID |
| `krun_set_vm_config(ctx, vcpus, ram_mib)` | Configures the VM resources |
| `krun_add_virtiofs(ctx, "/dev/root", rootfs)` | Mounts the build rootfs as `/` inside the VM (`"/dev/root"` = `KRUN_FS_ROOT_TAG`) |
| `krun_add_virtiofs(ctx, "krun-context", context)` | Exposes the build context directory to the VM |
| `krun_add_virtiofs(ctx, "krun-output", output_dir)` | Exposes the output directory to the VM |
| `krun_set_workdir(ctx, "/")` | Sets the working directory inside the VM |
| `krun_set_exec(ctx, "/bin/sh", argv, envp)` | Runs a shell script that mounts the virtiofs shares and invokes `buildah bud` |
| `krun_start_enter(ctx)` | Boots the VM — this call transfers control and does not return on success |

libkrun bundles its own Linux kernel via [libkrunfw](https://github.com/libkrun/homebrew-krun), so you do not need to supply or configure a kernel yourself.

The `krun-context` and `krun-output` virtiofs shares are not auto-mounted by the kernel — the shell script mounts them at `/build/context` and `/build/output` before running buildah. The Docker archive is written to `/build/output/<filename>` and appears on the host at the path given by `--output`.

## End-user prerequisites

The distributed binary dynamically loads libkrun at runtime, so end-users must install it:

```sh
brew tap libkrun/krun
brew install libkrun/krun/libkrun
```

The rootfs and the binary itself are included in the distribution package — nothing else is required.

## Development prerequisites

### 1. Rust toolchain

Install via [rustup](https://rustup.rs/):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. libkrun + libkrunfw

Use the official tap — the main Homebrew tap does not carry libkrun:

```sh
brew tap libkrun/krun
brew install libkrun/krun/libkrun
```

This installs:
- `libkrun.dylib` — the VM library itself, plus its C headers (needed by `krun-sys` at build time)
- `libkrunfw.dylib` — the bundled Linux kernel, pulled in automatically as a runtime dependency

### 3. LLVM (for bindgen)

`krun-sys` generates its FFI bindings at build time using [bindgen](https://github.com/rust-lang/rust-bindgen), which requires `libclang`:

```sh
brew install llvm
```

Add the following to your `~/.zshrc` so the build and runtime linker can find both LLVM and the krun libraries:

```sh
export DYLD_LIBRARY_PATH="$(brew --prefix)/lib:$(brew --prefix llvm)/lib:$DYLD_LIBRARY_PATH"
```

Then reload your shell:

```sh
source ~/.zshrc
```

### 4. A build rootfs with buildah

The VM boots using a Linux rootfs that must have `buildah` installed. The `vm-image/` directory provides a ready-to-use `Containerfile` (Fedora + buildah) and a script to build and export it:

```sh
chmod +x vm-image/make-rootfs.sh
./vm-image/make-rootfs.sh /tmp/krun-build-rootfs
```

This requires Podman. It builds a `linux/arm64` image and exports it to the given path.

The rootfs is pre-configured to use the `vfs` buildah storage driver — virtiofs does not support overlayfs, so the default overlay driver would fail inside the VM.

## Build and run

On macOS, processes that use `Hypervisor.framework` must be signed with the `com.apple.security.hypervisor` entitlement. The provided `run.sh` script handles this automatically:

```sh
chmod +x run.sh
./run.sh --rootfs /tmp/krun-build-rootfs -t myapp:latest ./myproject
```

It builds the binary, signs it with `entitlements.plist`, then runs it. You cannot use `cargo run` directly — every `cargo run` rebuilds the binary, which clears the code signature.

## Distribution

To produce a distributable package:

```sh
chmod +x dist.sh
./dist.sh
```

This creates a `dist/` directory:

```text
dist/
  krun-build-image        — wrapper script (quarantine removal, rootfs extraction)
  krun-build-image.bin    — release binary (signed)
  rootfs.tar.gz           — (add this manually) rootfs archive, extracted on first run
```

To produce `rootfs.tar.gz` from the directory exported by `make-rootfs.sh`:

```sh
tar -czf dist/rootfs.tar.gz -C /tmp/krun-build-rootfs .
```

Distribute the `dist/` directory as a zip or DMG. End-users must install libkrun first (see [End-user prerequisites](#end-user-prerequisites)). On first run the wrapper automatically strips the macOS quarantine flag from the binary and extracts `rootfs.tar.gz` into a `rootfs/` directory alongside the binary. Subsequent runs skip this setup. The binary uses `rootfs/` as the default root filesystem — no `--rootfs` flag required.

**For notarized distribution** (required for Gatekeeper on external machines without bypassing quarantine), replace `--sign -` in `dist.sh` with your Developer ID certificate and add `--options runtime`:

```sh
codesign --sign "Developer ID Application: You (TEAMID)" \
         --options runtime \
         --entitlements entitlements.plist \
         --force dist/krun-build-image.bin
```

Then notarize with `xcrun notarytool`.

## Project structure

```text
src/main.rs                   — VM setup and build orchestration
Cargo.toml                    — dependencies: krun-sys, libc, clap
entitlements.plist            — Hypervisor.framework entitlement for macOS signing
run.sh                        — build, sign, and run in one step (development)
dist.sh                       — build and sign for distribution
vm-image/
  Containerfile               — Fedora + buildah image for the build VM rootfs
  make-rootfs.sh              — exports the above as a rootfs directory (requires Podman)
```

## Troubleshooting

**`krun-sys` version mismatch** — if the crates.io release doesn't match your installed libkrun version, pin it to the repo directly:

```toml
[dependencies]
krun-sys = { git = "https://github.com/libkrun/libkrun" }
```

**`libkrunfw` not found at runtime** — libkrun loads its kernel via `dlopen` at runtime. Make sure `DYLD_LIBRARY_PATH` includes Homebrew's lib directory (see Prerequisites above), or that libkrun's own RPATH already points there.

**`pkg-config` can't find libkrun** — make sure Homebrew's prefix is on your `PKG_CONFIG_PATH`:

```sh
export PKG_CONFIG_PATH="$(brew --prefix)/lib/pkgconfig:$PKG_CONFIG_PATH"
```

**`mount: permission denied` inside the VM** — the build rootfs process must run as root inside the VM to mount virtiofs shares. Ensure the rootfs does not drop privileges before mounting.

**`buildah: command not found`** — the rootfs does not have buildah installed. See the Prerequisites section for how to prepare a suitable rootfs.
