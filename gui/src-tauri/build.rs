fn compile_accessibility_helper() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    use std::path::Path;
    use std::process::Command;

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let architecture = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => panic!("unsupported macOS target architecture"),
    };
    let deployment_target =
        std::env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "11.0".to_string());
    let source = Path::new(&manifest_dir).join("src/accessibility_helper.m");
    let object = Path::new(&out_dir).join("accessibility_helper.o");
    let library = Path::new(&out_dir).join("libaccessibility_helper.a");

    println!("cargo:rerun-if-changed={}", source.display());

    let status = Command::new("xcrun")
        .args(["--sdk", "macosx", "clang", "-fobjc-arc", "-fblocks"])
        .arg(format!("-mmacosx-version-min={deployment_target}"))
        .arg("-arch")
        .arg(architecture)
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .status()
        .expect("failed to start clang for the Accessibility helper");
    assert!(
        status.success(),
        "failed to compile the Accessibility helper"
    );

    let status = Command::new("libtool")
        .arg("-static")
        .arg("-o")
        .arg(&library)
        .arg(&object)
        .status()
        .expect("failed to start libtool for the Accessibility helper");
    assert!(status.success(), "failed to link the Accessibility helper");

    println!("cargo:rustc-link-search=native={out_dir}");
    println!("cargo:rustc-link-lib=static=accessibility_helper");
    println!("cargo:rustc-link-lib=framework=AppKit");
}

fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    #[cfg(target_os = "macos")]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let info_plist = std::path::Path::new(&manifest_dir).join("Info.plist");
        println!(
            "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            info_plist.display()
        );
    }
    compile_accessibility_helper();
    tauri_build::build();
}
