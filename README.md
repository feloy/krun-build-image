# krun-build-image

A Rust CLI that builds OCI images from a Containerfile inside a lightweight Linux microVM — using [libkrun](https://github.com/libkrun/libkrun).

## What it does

When you run `krun-build-image`, it:

1. Spins up a lightweight Linux microVM on your Mac using Apple's [Hypervisor.framework](https://developer.apple.com/documentation/hypervisor) — no Docker, no QEMU, no root required.
2. Mounts three host directories into the VM via [virtio-fs](https://virtio-fs.gitlab.io/): the build rootfs (as `/`), the build context, and the output directory.
3. Runs `buildah bud` inside the VM to build the image from the Containerfile.
4. Exports the resulting OCI image layout to the output directory on the host and exits.

The VM is fully isolated: it runs its own Linux kernel with its own process namespace, but shares no persistent state with your host.

## Usage

```sh
krun-build-image [OPTIONS] <CONTEXT>
```

| Argument / Option | Description | Default |
|---|---|---|
| `<CONTEXT>` | Build context directory | (required) |
| `--rootfs <DIR>` | VM root filesystem — must have `buildah` installed | (required) |
| `-f, --file <FILE>` | Path to the Containerfile | `<CONTEXT>/Containerfile` |
| `-o, --output <DIR>` | Output directory for the OCI image layout | `./output` |
| `-t, --tag <TAG>` | Image tag used by buildah | `krun-build` |
| `--cpus <N>` | Number of vCPUs | `2` |
| `--memory <MB>` | RAM in MiB | `2048` |

Example:

```sh
krun-build-image --rootfs /tmp/build-rootfs -t myapp:latest ./myproject
```

This builds the image from `./myproject/Containerfile` and writes an OCI image layout to `./output/`.

If the Containerfile is outside the context directory, pass its path explicitly:

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
| `krun_add_virtiofs(ctx, "krun-output", output)` | Exposes the output directory to the VM |
| `krun_set_workdir(ctx, "/")` | Sets the working directory inside the VM |
| `krun_set_exec(ctx, "/bin/sh", argv, envp)` | Runs a shell script that mounts the virtiofs shares and invokes `buildah bud` |
| `krun_start_enter(ctx)` | Boots the VM — this call transfers control and does not return on success |

libkrun bundles its own Linux kernel via [libkrunfw](https://github.com/libkrun/homebrew-krun), so you do not need to supply or configure a kernel yourself.

The `krun-context` and `krun-output` virtiofs shares are not auto-mounted by the kernel — the shell script mounts them at `/build/context` and `/build/output` before running buildah.

## Prerequisites

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
./run.sh --rootfs /tmp/build-rootfs -t myapp:latest ./myproject
```

It builds the binary, signs it with `entitlements.plist`, then runs it. You cannot use `cargo run` directly — every `cargo run` rebuilds the binary, which clears the code signature.

## Distribution

To produce a self-contained package that end-users can run without installing libkrun:

```sh
brew install dylibbundler   # one-time
chmod +x dist.sh
./dist.sh
```

This creates a `dist/` directory:

```text
dist/
  krun-build-image       — release binary (signed)
  libs/            — all dylib dependencies (libkrun, libkrunfw, etc.)
```

End-users need nothing installed. Distribute the `dist/` directory as a zip or DMG.

**Installing as an end-user** — unzip and run; the wrapper script handles the rest on first launch:

```sh
unzip krun-build-image-macos-arm64.zip -d krun-build-image
./krun-build-image/krun-build-image --rootfs /tmp/build-rootfs -t myapp:latest ./myproject
```

On first run the wrapper automatically makes the dylibs writable, strips the macOS quarantine flag from the binary and libraries, and extracts `rootfs.zip` if present. A `.setup-done` sentinel file prevents these steps from repeating on subsequent runs.

**For notarized distribution** (App Store / Gatekeeper), replace `--sign -` in `dist.sh` with your Developer ID certificate and add `--options runtime`:

```sh
codesign --sign "Developer ID Application: You (TEAMID)" \
         --options runtime \
         --entitlements entitlements.plist \
         --force dist/krun-build-image
```

Then notarize with `xcrun notarytool`.

### Why libkrunfw needs special handling

`libkrun` loads its kernel (`libkrunfw`) via `dlopen()` at runtime using just the filename — it is not a static link-time dependency, so `dylibbundler` won't see it. `dist.sh` copies it manually and `src/main.rs` pre-loads it as `RTLD_GLOBAL` before any libkrun call, so that libkrun's own `dlopen()` finds it already in memory. This avoids needing `DYLD_LIBRARY_PATH` (which is stripped under hardened runtime).

## Project structure

```text
src/main.rs                   — VM setup, build orchestration, libkrunfw pre-loader
Cargo.toml                    — dependencies: krun-sys, libc, clap
entitlements.plist            — Hypervisor.framework entitlement for macOS signing
run.sh                        — build, sign, and run in one step (development)
dist.sh                       — build, bundle dylibs, sign (distribution)
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

**`libkrunfw` not found at runtime** — libkrun loads the kernel via `dlopen` at runtime, so it needs `DYLD_LIBRARY_PATH` to include the Homebrew lib directory (see Prerequisites above).

**`pkg-config` can't find libkrun** — make sure Homebrew's prefix is on your `PKG_CONFIG_PATH`:

```sh
export PKG_CONFIG_PATH="$(brew --prefix)/lib/pkgconfig:$PKG_CONFIG_PATH"
```

**`mount: permission denied` inside the VM** — the build rootfs process must run as root inside the VM to mount virtiofs shares. Ensure the rootfs does not drop privileges before mounting.

**`buildah: command not found`** — the rootfs does not have buildah installed. See the Prerequisites section for how to prepare a suitable rootfs.
