fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=startup.order");
    println!("cargo:rerun-if-env-changed=SCRUTATE_LINUX_ORDER");
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    if env("CARGO_CFG_TARGET_OS") == "macos" {
        // the standard library links libiconv and the executable calls none of it: one library
        // fewer for dyld to load (0.18 M instructions at every start)
        println!("cargo:rustc-link-arg-bins=-Wl,-dead_strip_dylibs");
        // the functions a small search runs, side by side (tools/order_file.py)
        let order = std::path::Path::new(&env("CARGO_MANIFEST_DIR")).join("startup.order");
        if order.is_file() {
            println!("cargo:rustc-link-arg-bins=-Wl,-order_file,{}", order.display());
        }
    }
    // The same order on glibc Linux, from a list `tools/order_file.py --elf` maps from a first
    // build's names: crate hashes in Rust names differ between targets.
    let order = env("SCRUTATE_LINUX_ORDER");
    if env("CARGO_CFG_TARGET_OS") == "linux" && env("CARGO_CFG_TARGET_ENV") == "gnu" && !order.is_empty() {
        println!("cargo:rerun-if-changed={order}");
        if !std::path::Path::new(&order).is_file() {
            return;
        }
        // GNU ld has no symbol order. rustc on x86_64 Linux already links with its own rust-lld
        // this way; elsewhere gcc would pick GNU ld, so name the toolchain's rust-lld every time.
        let sysroot = std::process::Command::new(env("RUSTC")).args(["--print", "sysroot"]).output();
        let gcc_ld = sysroot.ok().map(|o| {
            let root = String::from_utf8_lossy(&o.stdout).trim().to_string();
            std::path::Path::new(&root).join("lib/rustlib").join(env("HOST")).join("bin/gcc-ld")
        });
        match gcc_ld {
            Some(dir) if dir.join("ld.lld").is_file() => {
                println!("cargo:rustc-link-arg-bins=-fuse-ld=lld");
                println!("cargo:rustc-link-arg-bins=-B{}", dir.display());
            }
            _ => {
                println!("cargo:warning=no rust-lld in the toolchain: linking {} without the function order", env("TARGET"));
                return;
            }
        }
        // a name the link does not have is passed over; lld would warn once for each
        println!("cargo:rustc-link-arg-bins=-Wl,--symbol-ordering-file={order}");
        println!("cargo:rustc-link-arg-bins=-Wl,--no-warn-symbol-ordering");
    }
}
