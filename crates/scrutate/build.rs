fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=startup.order");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // the standard library links libiconv and the executable calls none of it: one library
        // fewer for dyld to load (0.18 M instructions at every start)
        println!("cargo:rustc-link-arg-bins=-Wl,-dead_strip_dylibs");
        // the functions a small search runs, side by side (tools/order_file.py)
        let order = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("startup.order");
        if order.is_file() {
            println!("cargo:rustc-link-arg-bins=-Wl,-order_file,{}", order.display());
        }
    }
}
