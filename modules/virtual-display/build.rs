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
            .args(["--target", "libvirtualdisplay_driver", "-j10"])
            .status()
            .expect("could not build libvirtualdisplay")
            .success(),
        "libvirtualdisplay build failed"
    );
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
}
