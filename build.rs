use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib_dir = manifest.join("vendor/ghostty-vt/lib");

    println!("cargo:rerun-if-changed=vendor/ghostty-vt/lib/libghostty-vt.a");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt");

    // Ghostty's VT library is a Zig build that bundles compiler-rt and uses
    // libc; the embedded Highway SIMD kernels are C++.
    println!("cargo:rustc-link-lib=dylib=m");
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-link-lib=dylib=pthread");
    }
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=dylib=c++");
    }
}
