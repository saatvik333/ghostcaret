//! Hands the directory of the freshly built llama.cpp backend modules to the
//! crate, so a development run finds them without packaging. `cargo:rustc-env`
//! from `llama-cpp-sys-2` only reaches that crate; this build script forwards it.

fn main() {
    if let Ok(dir) = std::env::var("DEP_LLAMA_BACKENDS_DIR") {
        println!("cargo:rustc-env=GGML_BACKENDS_DIR={dir}");
    }
}
