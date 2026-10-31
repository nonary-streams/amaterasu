use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(amaterasu_libvirtualdisplay)");
    println!("cargo:rerun-if-changed=src/linux_control/adapter.cpp");
    if env::var_os("CARGO_FEATURE_LINUX_LIBVIRTUALDISPLAY").is_none()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
    {
        return;
    }
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../third-party/libvirtualdisplay");
    assert!(
        source.join("CMakeLists.txt").is_file(),
        "initialize the pinned third-party/libvirtualdisplay submodule before building"
    );
    for path in ["CMakeLists.txt", "src/CMakeLists.txt", "src/driver"] {
        println!("cargo:rerun-if-changed={}", source.join(path).display());
    }
    let build = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("libvirtualdisplay");
    let status = Command::new("cmake")
        .arg("-S")
        .arg(&source)
        .arg("-B")
        .arg(&build)
        .args([
            "-G",
            "Ninja",
            "-DBUILD_TESTS=OFF",
            "-DCMAKE_BUILD_TYPE=Release",
            "-DCMAKE_POSITION_INDEPENDENT_CODE=ON",
        ])
        .status()
        .expect("libvirtualdisplay requires CMake and Ninja");
    assert!(status.success(), "libvirtualdisplay configuration failed");
    assert!(
        Command::new("cmake")
            .arg("--build")
            .arg(&build)
            .args(["--target", "libvirtualdisplay_driver", "-j2"])
            .status()
            .expect("could not build libvirtualdisplay")
            .success(),
        "libvirtualdisplay build failed"
    );
    let wayland = Command::new("pkg-config")
        .args(["--cflags", "--libs", "wayland-client"])
        .output()
        .expect("Wayland pkg-config required");
    assert!(
        wayland.status.success(),
        "linux-libvirtualdisplay requires wayland-client development files"
    );
    let mut observer = cc::Build::new();
    observer
        .file("src/linux_manager/wayland_observer.c")
        .flag("-std=c11");
    for flag in String::from_utf8(wayland.stdout)
        .unwrap()
        .split_whitespace()
    {
        if let Some(path) = flag.strip_prefix("-I") {
            observer.include(path);
        } else if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={lib}");
        } else if let Some(path) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={path}");
        } else {
            observer.flag(flag);
        }
    }
    observer.compile("amaterasu_wayland_observer");
    println!("cargo:rerun-if-changed=src/linux_manager/wayland_observer.c");
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .include(source.join("src/driver/include"))
        .file("src/linux_control/adapter.cpp")
        .compile("amaterasu_virtual_display_adapter");
    println!(
        "cargo:rustc-link-search=native={}",
        build.join("src/driver").display()
    );
    println!("cargo:rustc-link-lib=static=libvirtualdisplay_driver");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rustc-cfg=amaterasu_libvirtualdisplay");
    println!("cargo:rustc-link-arg-cdylib=-Wl,--undefined=amaterasu_linux_vdd_marker");
}
