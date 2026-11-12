fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // the standard library links libiconv and the executable calls none of it: one library
        // fewer for dyld to load (0.18 M instructions at every start)
        println!("cargo:rustc-link-arg-bins=-Wl,-dead_strip_dylibs");
    }
}
