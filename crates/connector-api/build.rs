//! Makes the shared library relocatable, as the distribution package needs:
//!
//! - it is named by its file name alone, so that a binary linked against it
//!   finds it through its own run-time search path wherever the library is
//!   installed, instead of by the path it was built at; and
//! - it searches its own directory, where the package places the native
//!   libraries it links that ship with it (packaging/README.md).

fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            println!("cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libconnector_api.dylib");
            println!("cargo:rustc-cdylib-link-arg=-Wl,-rpath,@loader_path");
        }
        Ok("linux") => {
            println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libconnector_api.so");
            println!("cargo:rustc-cdylib-link-arg=-Wl,-rpath,$ORIGIN");
        }
        _ => {}
    }
}
