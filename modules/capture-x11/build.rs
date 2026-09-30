use std::{env, process::Command};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(amaterasu_x11)");
    println!("cargo:rerun-if-changed=src/platform/adapter.c");
    println!("cargo:rerun-if-changed=src/platform/adapter.h");
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
        .args(["--cflags", "--libs", "xcb", "xcb-randr"])
        .output()
    else {
        println!("cargo:warning=X11 capture disabled: pkg-config unavailable");
        return;
    };
    if !output.status.success() {
        println!(
            "cargo:warning=X11 capture disabled: XCB/XRandR development dependencies unavailable"
        );
        return;
    }
    let mut build = cc::Build::new();
    build.file("src/platform/adapter.c").flag("-std=c11");
    for flag in String::from_utf8(output.stdout)
        .expect("pkg-config output is UTF-8")
        .split_whitespace()
    {
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
    build.warnings(true).compile("amaterasu_x11_adapter");
    println!("cargo:rustc-cfg=amaterasu_x11");
}
