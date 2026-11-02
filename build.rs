use std::env;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

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

    // Build the Swift Foundation Models bridge on macOS. It uses
    // `#if canImport(FoundationModels)` so the same source compiles on older
    // SDKs; at runtime the bridge reports itself unavailable and Prompt falls
    // back to Qwen.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build_foundation_models(&manifest);
    }
}

#[cfg(not(target_os = "macos"))]
fn build_foundation_models(_: &Path) {}

#[cfg(target_os = "macos")]
fn build_foundation_models(manifest: &Path) {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let swift = manifest.join("src/foundation_models.swift");
    let stub = manifest.join("src/foundation_models_stub.c");
    let object = out.join("foundation_models.o");
    let library = out.join("libprompt_foundation_models.a");

    println!("cargo:rerun-if-changed={}", swift.display());
    println!("cargo:rerun-if-changed={}", stub.display());

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64" | "arm64") => "arm64",
        _ => "x86_64",
    };
    let sdk = run_output(Command::new("xcrun").args(["--sdk", "macosx", "--show-sdk-path"]));
    let swiftc = run_output(Command::new("xcrun").args(["--find", "swiftc"]));

    let deployment = env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "12.0".into());
    let mut built = false;
    if let (Some(sdk), Some(swiftc)) = (&sdk, &swiftc) {
        let status = Command::new(swiftc)
            .args(["-parse-as-library", "-emit-object", "-O"])
            .args(["-module-name", "PromptFoundationModels"])
            .arg("-target")
            .arg(format!("{arch}-apple-macos{deployment}"))
            .arg("-sdk")
            .arg(sdk)
            .arg("-o")
            .arg(&object)
            .arg(&swift)
            .status();
        built = matches!(status, Ok(status) if status.success());
    }

    if built && archive(&library, &object) {
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=prompt_foundation_models");
        // Swift's runtime lives in the OS on macOS 10.14.4+.
        println!("cargo:rustc-link-search=native=/usr/lib/swift");
        if let Some(sdk) = &sdk {
            println!(
                "cargo:rustc-link-search=native={}",
                sdk.join("usr/lib/swift").display()
            );
        }
        println!("cargo:rustc-link-lib=dylib=swiftCore");
        println!("cargo:rustc-link-lib=dylib=swift_Concurrency");

        // Weak-link the framework so a binary built with the macOS 27 SDK
        // still launches on older systems.
        if let Some(sdk) = &sdk {
            if sdk
                .join("System/Library/Frameworks/FoundationModels.framework")
                .exists()
            {
                println!("cargo:rustc-link-arg=-Wl,-weak_framework");
                println!("cargo:rustc-link-arg=FoundationModels");
            }
        }
        return;
    }

    // Older SDK, or no Swift toolchain. Keep the build working and report the
    // feature as unavailable at runtime.
    println!("cargo:warning=Foundation Models bridge not built; Prompt will use Qwen on this Mac");
    let status = Command::new(env::var("CC").unwrap_or_else(|_| "cc".into()))
        .arg("-c")
        .arg(&stub)
        .arg("-o")
        .arg(&object)
        .status();
    if matches!(status, Ok(status) if status.success()) && archive(&library, &object) {
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=prompt_foundation_models");
    }
}

#[cfg(target_os = "macos")]
fn archive(library: &Path, object: &Path) -> bool {
    let _ = std::fs::remove_file(library);
    matches!(
        Command::new("ar")
            .arg("crs")
            .arg(library)
            .arg(object)
            .status(),
        Ok(status) if status.success()
    )
}

#[cfg(target_os = "macos")]
fn run_output(command: &mut Command) -> Option<PathBuf> {
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}
