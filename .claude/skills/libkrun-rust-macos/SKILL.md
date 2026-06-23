---
name: libkrun-rust-macos
description: Scaffold and explain Rust apps that use libkrun to run processes in lightweight Linux microVMs on macOS Apple Silicon. Covers installation, project setup, API usage, code signing, and running. Use when the user wants to build a Rust app with libkrun on macOS.
---

When helping the user build a Rust app with libkrun on macOS, follow all of the steps below precisely. Every step reflects a hard-won lesson — skipping any of them leads to a specific, non-obvious failure.

## 1. Prerequisites

### libkrun (official tap — not the main Homebrew tap)

```sh
brew tap libkrun/krun
brew install libkrun/krun/libkrun
```

This also installs `libkrunfw` (the bundled Linux kernel) as a runtime dependency. Do NOT use `brew install libkrun` from the main Homebrew tap — it doesn't exist there or installs the wrong thing.

### LLVM (required by bindgen at build time)

`krun-sys` generates its FFI bindings at build time using bindgen, which needs `libclang`:

```sh
brew install llvm
```

Add to `~/.zshrc`:

```sh
export DYLD_LIBRARY_PATH="$(brew --prefix)/lib:$(brew --prefix llvm)/lib:$DYLD_LIBRARY_PATH"
```

Then `source ~/.zshrc`.

`DYLD_LIBRARY_PATH` serves two purposes:
- `$(brew --prefix llvm)/lib` — lets bindgen find `libclang.dylib` at build time
- `$(brew --prefix)/lib` — lets libkrun find `libkrunfw.dylib` via `dlopen` at runtime

### Linux root filesystem

The VM needs a Linux rootfs directory. Quickest way using Podman + Alpine (must be arm64):

```sh
podman create --platform linux/arm64 --name tmp alpine sh
mkdir -p /tmp/rootfs
podman export tmp | tar -C /tmp/rootfs -x
podman rm tmp
```

---

## 2. Project structure

```
Cargo.toml          — depends on krun-sys = "1"
src/main.rs         — VM setup and boot
entitlements.plist  — Hypervisor.framework entitlement
run.sh              — build + sign + run (replaces cargo run)
```

### Cargo.toml

```toml
[package]
name = "my-krun-app"
version = "0.1.0"
edition = "2021"

[dependencies]
krun-sys = "1"
```

No `build.rs` needed — `krun-sys` handles linking via its own build script.

If the crates.io release of `krun-sys` doesn't match the installed libkrun version, pin to git:

```toml
krun-sys = { git = "https://github.com/libkrun/libkrun" }
```

---

## 3. Code signing — mandatory

On macOS, any process using `Hypervisor.framework` must be signed with the `com.apple.security.hypervisor` entitlement. Without it, `krun_start_enter` returns `-22` (EINVAL / VmCreate failure).

**`entitlements.plist`:**

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.hypervisor</key>
    <true/>
</dict>
</plist>
```

**`run.sh`** (use this instead of `cargo run`):

```sh
#!/bin/sh
set -e
cargo build
codesign --sign - --entitlements entitlements.plist --force target/debug/my-krun-app
DYLD_LIBRARY_PATH="$(brew --prefix)/lib:${DYLD_LIBRARY_PATH}" ./target/debug/my-krun-app "$@"
```

`cargo run` cannot be used — it rebuilds the binary, which strips the signature. Always use `run.sh`.

---

## 4. libkrun API — key facts

Use `krun-sys` (bindgen-generated FFI bindings). All calls are `unsafe`. Call them in this order:

| Function | Notes |
|----------|-------|
| `krun_create_ctx()` | Returns a non-negative ctx_id on success, negative on error |
| `krun_set_vm_config(ctx, vcpus, ram_mib)` | 1 vCPU + 512 MiB is enough for most workloads |
| `krun_add_virtiofs(ctx, tag, path)` | **tag must be `"/dev/root"` (`KRUN_FS_ROOT_TAG`)** — NOT `"root"`. Wrong tag causes kernel panic: "VFS: Unable to mount root fs" |
| `krun_set_workdir(ctx, "/")` | Working directory inside the VM |
| `krun_set_exec(ctx, exec_path, argv, envp)` | Binary path is inside the VM filesystem |
| `krun_start_enter(ctx)` | Boots the VM — never returns on success |

**Do NOT call `krun_add_virtio_console_default`** for plain stdin/stdout/stderr passthrough. libkrun auto-detects the process's stdio and wires it to the VM console automatically (the same path used by krunvm). Calling this function explicitly bypasses the auto-configure path, which is the path that correctly saves and restores terminal state via libkrun's exit observer before `_exit()`. If you call it, the host terminal is left in raw mode after the VM exits — typed characters become invisible and output loses carriage returns.

**Minimal working example:**

```rust
use std::ffi::CString;
use std::os::raw::c_char;
use std::ptr;

use krun_sys::{
    krun_add_virtiofs, krun_create_ctx, krun_set_exec,
    krun_set_vm_config, krun_set_workdir, krun_start_enter,
};

fn check(call: &str, ret: i32) {
    if ret < 0 {
        eprintln!("libkrun: {} failed (code {})", call, ret);
        std::process::exit(1);
    }
}

fn main() {
    let rootfs = std::env::args().nth(1).expect("Usage: app <rootfs-path>");

    unsafe {
        let ctx_id = krun_create_ctx();
        assert!(ctx_id >= 0, "krun_create_ctx failed: {}", ctx_id);
        let ctx_id = ctx_id as u32;

        check("krun_set_vm_config", krun_set_vm_config(ctx_id, 1, 512));

        // KRUN_FS_ROOT_TAG = "/dev/root" — this is the root filesystem
        let tag = CString::new("/dev/root").unwrap();
        let path = CString::new(rootfs.as_str()).unwrap();
        check("krun_add_virtiofs", krun_add_virtiofs(ctx_id, tag.as_ptr(), path.as_ptr()));

        let workdir = CString::new("/").unwrap();
        check("krun_set_workdir", krun_set_workdir(ctx_id, workdir.as_ptr()));

        let exec = CString::new("/bin/echo").unwrap();
        let arg0 = CString::new("echo").unwrap();
        let arg1 = CString::new("Hello from libkrun VM!").unwrap();
        let argv: &[*const c_char] = &[arg0.as_ptr(), arg1.as_ptr(), ptr::null()];

        let env_path = CString::new("PATH=/bin:/usr/bin").unwrap();
        let envp: &[*const c_char] = &[env_path.as_ptr(), ptr::null()];

        check("krun_set_exec",
            krun_set_exec(ctx_id, exec.as_ptr(), argv.as_ptr(), envp.as_ptr()));

        let ret = krun_start_enter(ctx_id);
        eprintln!("VM exited with code: {}", ret);
    }
}
```

---

## 5. Debugging

To enable verbose libkrun logs, add `krun_init_log` at the top of the `unsafe` block:

```rust
use krun_sys::krun_init_log;
// ...
krun_init_log(2, 5, 0, 0); // stderr, trace level
```

Remove it once the issue is found — it is very noisy.

---

## 6. Common failure modes

| Symptom | Cause | Fix |
|---------|-------|-----|
| `dyld: Library not loaded: libclang.dylib` | bindgen can't find LLVM | `brew install llvm` + set `DYLD_LIBRARY_PATH` |
| `Couldn't find or load libkrunfw.5.dylib` | `libkrunfw` not on dlopen path | Set `DYLD_LIBRARY_PATH="$(brew --prefix)/lib:..."` in `run.sh` |
| `VmCreate` / exit code -22 | Missing `com.apple.security.hypervisor` entitlement | Sign binary with `entitlements.plist` using `codesign` |
| Kernel panic: `VFS: Unable to mount root fs` | Wrong virtiofs root tag | Use `"/dev/root"`, not `"root"` |
| Code signature lost after rebuild | Used `cargo run` instead of `run.sh` | Always use `run.sh` |
| Terminal broken after VM exits (no echo, no carriage returns) | Called `krun_add_virtio_console_default` explicitly | Remove the call — libkrun auto-configures the console and correctly restores terminal state |
