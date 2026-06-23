use std::ffi::CString;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::ptr;

use clap::Parser;
use krun_sys::{
    krun_add_virtiofs, krun_create_ctx, krun_set_exec,
    krun_set_log_level, krun_set_vm_config, krun_set_workdir, krun_start_enter,
};

// dlopen is in libSystem on macOS — always linked, no extra dependency needed.
extern "C" {
    fn dlopen(filename: *const c_char, flag: i32) -> *mut std::ffi::c_void;
}
const RTLD_NOW: i32 = 0x2;
const RTLD_GLOBAL: i32 = 0x8;

// libkrun loads libkrunfw via dlopen() at runtime using just the filename, so
// dylibbundler won't see it as a static dependency and DYLD_LIBRARY_PATH won't
// help under hardened runtime. We pre-load it as RTLD_GLOBAL from the bundled
// libs/ directory so that libkrun's own dlopen() finds it already in memory.
// During development (no libs/ dir next to the binary) this is a no-op.
fn preload_krunfw() {
    let Ok(exe) = std::env::current_exe() else { return };
    let libs = exe.parent().unwrap_or(Path::new(".")).join("libs");
    let Ok(entries) = std::fs::read_dir(&libs) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.starts_with("libkrunfw") && s.ends_with(".dylib") {
            if let Some(p) = entry.path().to_str().and_then(|s| CString::new(s).ok()) {
                unsafe { dlopen(p.as_ptr(), RTLD_NOW | RTLD_GLOBAL); }
            }
            break;
        }
    }
}

fn debug_enabled() -> bool {
    std::env::var("KRUN_DEBUG").is_ok()
}

macro_rules! dbg_log {
    ($($arg:tt)*) => {
        if debug_enabled() {
            eprintln!("[krun-build-image] {}", format!($($arg)*));
        }
    };
}

fn check(call: &str, ret: i32) {
    if ret < 0 {
        eprintln!("libkrun: {} failed (code {})", call, ret);
        std::process::exit(1);
    }
    dbg_log!("{} -> {}", call, ret);
}

/// Build an OCI image from a Containerfile inside a libkrun microVM.
///
/// The VM boots using the provided rootfs (which must have buildah installed),
/// mounts the build context and output directory via virtio-fs, and runs
/// buildah to produce an OCI image layout at the output path.
#[derive(Parser)]
#[command(name = "krun-build-image")]
struct Cli {
    /// Build context directory
    context: PathBuf,

    /// Path to the Containerfile [default: <CONTEXT>/Containerfile]
    #[arg(long, short = 'f')]
    file: Option<PathBuf>,

    /// Path to the VM root filesystem (must have buildah installed)
    #[arg(long)]
    rootfs: PathBuf,

    /// Output directory for the OCI image layout
    #[arg(long, short = 'o', default_value = "output")]
    output: PathBuf,

    /// Image tag
    #[arg(long, short = 't', default_value = "krun-build")]
    tag: String,

    /// Number of vCPUs
    #[arg(long, default_value = "2")]
    cpus: u8,

    /// Memory in MiB
    #[arg(long, default_value = "4096")]
    memory: u32,
}

fn main() {
    preload_krunfw();

    if debug_enabled() {
        // 5 = trace — lets libkrun emit its own internal logs to stderr.
        unsafe { krun_set_log_level(5) };
    }

    let cli = Cli::parse();

    // Resolve and validate all paths before touching libkrun.
    let context = cli.context.canonicalize().unwrap_or_else(|e| {
        eprintln!("error: context directory '{}': {}", cli.context.display(), e);
        std::process::exit(1);
    });

    let rootfs = cli.rootfs.canonicalize().unwrap_or_else(|e| {
        eprintln!("error: rootfs '{}': {}", cli.rootfs.display(), e);
        std::process::exit(1);
    });

    let containerfile = match &cli.file {
        Some(p) => p.canonicalize().unwrap_or_else(|e| {
            eprintln!("error: Containerfile '{}': {}", p.display(), e);
            std::process::exit(1);
        }),
        None => {
            let default = context.join("Containerfile");
            if !default.exists() {
                eprintln!(
                    "error: no Containerfile found in '{}'. Use -f to specify one.",
                    context.display()
                );
                std::process::exit(1);
            }
            default
        }
    };

    std::fs::create_dir_all(&cli.output).unwrap_or_else(|e| {
        eprintln!("error: creating output directory '{}': {}", cli.output.display(), e);
        std::process::exit(1);
    });
    let output = cli.output.canonicalize().unwrap_or_else(|e| {
        eprintln!("error: output directory '{}': {}", cli.output.display(), e);
        std::process::exit(1);
    });

    // Determine the Containerfile path as seen inside the VM.
    // If it lives within the context directory it is reachable via the context mount;
    // otherwise mount its parent directory under a separate virtiofs tag.
    let cfile_outside_context = !containerfile.starts_with(&context);
    let (cfile_vm_path, cfile_host_dir): (String, Option<PathBuf>) = if cfile_outside_context {
        let parent = containerfile.parent().unwrap().to_path_buf();
        let name = containerfile.file_name().unwrap().to_string_lossy().into_owned();
        (format!("/build/cfile/{name}"), Some(parent))
    } else {
        let rel = containerfile.strip_prefix(&context).unwrap();
        (format!("/build/context/{}", rel.display()), None)
    };

    // libkrun encodes argv into the Linux kernel cmdline, which uses spaces as
    // delimiters. Passing a shell one-liner with spaces would silently split the
    // arguments. Instead, exec a pre-baked script (/usr/local/bin/krun-build)
    // that lives in the build rootfs and accepts simple space-free arguments.
    dbg_log!(
        "isatty: stdin={} stdout={} stderr={}",
        unsafe { libc::isatty(0) },
        unsafe { libc::isatty(1) },
        unsafe { libc::isatty(2) },
    );

    println!(
        "Building OCI image (context: {}, Containerfile: {}, output: {})...",
        context.display(),
        containerfile.display(),
        output.display(),
    );

    // Convert host paths to CStrings. All bindings must outlive both unsafe blocks
    // below (krun_set_exec stores pointers; krun_start_enter consumes them).
    let rootfs_c    = CString::new(rootfs.to_str().unwrap()).unwrap();
    let context_c   = CString::new(context.to_str().unwrap()).unwrap();
    let output_c    = CString::new(output.to_str().unwrap()).unwrap();
    let cfile_dir_c = cfile_host_dir
        .as_ref()
        .map(|p| CString::new(p.to_str().unwrap()).unwrap());

    let tag_root    = CString::new("/dev/root").unwrap();
    let tag_context = CString::new("krun-context").unwrap();
    let tag_output  = CString::new("krun-output").unwrap();
    let tag_cfile   = CString::new("krun-cfile").unwrap();

    let workdir      = CString::new("/").unwrap();
    let exec_path    = CString::new("/usr/local/bin/krun-build").unwrap();
    let arg_cfile    = CString::new(cfile_vm_path.as_str()).unwrap();
    let arg_tag      = CString::new(cli.tag.as_str()).unwrap();
    let arg_outside  = CString::new(if cfile_outside_context { "1" } else { "0" }).unwrap();
    let env_path     = CString::new(
        "PATH=/bin:/usr/bin:/usr/local/bin:/sbin:/usr/sbin",
    ).unwrap();

    let ctx_id = unsafe {
        let ctx_id = krun_create_ctx();
        assert!(ctx_id >= 0, "krun_create_ctx failed: {}", ctx_id);
        let ctx_id = ctx_id as u32;
        dbg_log!("krun_create_ctx -> ctx_id={}", ctx_id);

        check("krun_set_vm_config", krun_set_vm_config(ctx_id, cli.cpus, cli.memory));

        // /dev/root is KRUN_FS_ROOT_TAG — the bundled kernel mounts this as /.
        check("krun_add_virtiofs(root)",    krun_add_virtiofs(ctx_id, tag_root.as_ptr(),    rootfs_c.as_ptr()));
        check("krun_add_virtiofs(context)", krun_add_virtiofs(ctx_id, tag_context.as_ptr(), context_c.as_ptr()));
        check("krun_add_virtiofs(output)",  krun_add_virtiofs(ctx_id, tag_output.as_ptr(),  output_c.as_ptr()));

        if let Some(ref cfile_c) = cfile_dir_c {
            check("krun_add_virtiofs(cfile)", krun_add_virtiofs(ctx_id, tag_cfile.as_ptr(), cfile_c.as_ptr()));
        }

        check("krun_set_workdir", krun_set_workdir(ctx_id, workdir.as_ptr()));

        // The kernel's shebang mechanism prepends the script path as argv[0] when
        // exec'ing an interpreted script, so our array starts at what will be $1.
        let argv: &[*const c_char] = &[
            arg_cfile.as_ptr(),   // $1 — Containerfile path inside the VM
            arg_tag.as_ptr(),     // $2 — image tag
            arg_outside.as_ptr(), // $3 — "1" if Containerfile is outside context
            ptr::null(),
        ];
        let envp: &[*const c_char] = &[env_path.as_ptr(), ptr::null()];
        check("krun_set_exec", krun_set_exec(ctx_id, exec_path.as_ptr(), argv.as_ptr(), envp.as_ptr()));

        ctx_id
    };

    dbg_log!("calling krun_start_enter(ctx_id={})", ctx_id);
    let ret = unsafe { krun_start_enter(ctx_id) };
    if ret != 0 {
        eprintln!("build failed (VM exit code: {})", ret);
        std::process::exit(1);
    }
}
