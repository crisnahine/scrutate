#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod lazy_bind;

fn main() {
    scrutate::cli::main_exit();
}
