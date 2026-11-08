use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(amaterasu_kms)");
    for path in [
        "src/platform/adapter.c",
        "src/platform/adapter.h",
        "src/platform/cleanup_tests.c",
        "src/platform/helper.c",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for variable in [
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_SYSROOT_DIR",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        || env::var("HOST").ok() != env::var("TARGET").ok()
    {
        return;
    }
    let Ok(output) = Command::new("pkg-config")
        .args(["--cflags", "--libs", "libdrm", "gbm", "egl", "glesv2"])
        .output()
    else {
        println!("cargo:warning=KMS capture disabled: pkg-config unavailable");
        return;
    };
    if !output.status.success() {
        println!(
            "cargo:warning=KMS capture disabled: libdrm, GBM, EGL, or GLES development dependencies unavailable"
        );
        return;
    }
    let mut build = cc::Build::new();
    build
        .file("src/platform/adapter.c")
        .file("src/platform/cleanup_tests.c")
        .flag("-std=c11");
    let flags = String::from_utf8(output.stdout).expect("pkg-config output is UTF-8");
    for flag in flags.split_whitespace() {
        if let Some(directory) = flag.strip_prefix("-I") {
            build.include(directory);
        } else if let Some(library) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={library}");
        } else if let Some(directory) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={directory}");
        } else {
            build.flag(flag);
        }
    }
    build.warnings(true).compile("amaterasu_kms_adapter");

    if env::var_os("CARGO_FEATURE_ISOLATED_HELPER").is_some() {
        // This development candidate is not registered in the provider yet.
        // Once integrated, its exact bytes will be bound into the signed module.
        let helper =
            PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("amaterasu-kms-helper");
        let mut command = build.get_compiler().to_command();
        command
            .arg("-std=c11")
            .arg("-pthread")
            .arg("src/platform/adapter.c")
            .arg("src/platform/helper.c")
            .arg("-o")
            .arg(&helper)
            .arg("-Wl,-z,relro,-z,now");
        command.args(flags.split_whitespace());
        assert!(
            command
                .status()
                .expect("could not compile KMS helper")
                .success(),
            "could not compile KMS helper"
        );
    }
    println!("cargo:rustc-cfg=amaterasu_kms");
}
