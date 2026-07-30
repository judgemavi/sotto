use std::{env, error::Error, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=bridge-macos/Package.swift");
    println!("cargo:rerun-if-changed=bridge-macos/Sources/SottoCaptureBridge/CaptureBridge.swift");
    println!("cargo:rerun-if-changed=bridge-macos/include/SottoCaptureBridge.h");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return Ok(());
    }
    if env::var_os("SOTTO_SKIP_SWIFT_BUILD").is_some() {
        return Ok(());
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let bridge_dir = manifest_dir.join("bridge-macos");
    let status = Command::new("swift")
        .args(["build", "-c", "release"])
        .current_dir(&bridge_dir)
        .status();

    match status? {
        exit if exit.success() => {}
        exit => return Err(format!("Swift capture bridge build failed with {exit}").into()),
    }

    let rust_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let swift_arch = if rust_arch == "aarch64" {
        "arm64"
    } else {
        &rust_arch
    };
    let library_dir = bridge_dir
        .join(".build")
        .join(format!("{swift_arch}-apple-macosx"))
        .join("release");
    println!("cargo:rustc-link-search=native={}", library_dir.display());
    println!("cargo:rustc-link-lib=static=SottoCaptureBridge");
    for framework in [
        "ScreenCaptureKit",
        "CoreMedia",
        "CoreVideo",
        "CoreGraphics",
        "AVFoundation",
        "AppKit",
        "Foundation",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    Ok(())
}
