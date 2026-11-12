use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    for path in [
        "build.rs",
        "src/resolver/helper_main.rs",
        "src/resolver/helperprotocol.rs",
        "src/resolver/helper_fixture.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for name in ["RUSTC", "TARGET", "RUSTC_LINKER", "CARGO_ENCODED_RUSTFLAGS"] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let rustc = env::var_os("RUSTC").expect("Cargo must provide RUSTC for the DNS helper");
    let target = env::var_os("TARGET").expect("Cargo must provide TARGET for the DNS helper");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output_directory = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let suffix = if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        ".exe"
    } else {
        ""
    };
    for (name, source, environment) in [
        (
            "amaterasu_network_resolver_helper",
            "helper_main.rs",
            "AMATERASU_NETWORK_RESOLVER_HELPER_PATH",
        ),
        (
            "amaterasu_network_resolver_fixture",
            "helper_fixture.rs",
            "AMATERASU_NETWORK_RESOLVER_FIXTURE_PATH",
        ),
    ] {
        let executable = output_directory.join(format!("{name}{suffix}"));
        compile_executable(
            &rustc,
            &target,
            &manifest.join("src/resolver").join(source),
            &executable,
            name,
        );
        println!(
            "cargo:rustc-env={environment}={}",
            executable
                .to_str()
                .expect("DNS helper output path must be UTF-8")
        );
    }
}

fn compile_executable(rustc: &OsStr, target: &OsStr, source: &Path, executable: &Path, name: &str) {
    let mut compiler = Command::new(rustc);
    if let Some(flags) = env::var_os("CARGO_ENCODED_RUSTFLAGS") {
        for flag in flags
            .to_string_lossy()
            .split('\u{1f}')
            .filter(|flag| !flag.is_empty())
        {
            compiler.arg(flag);
        }
    }
    if let Some(linker) = env::var_os("RUSTC_LINKER") {
        let mut option = std::ffi::OsString::from("linker=");
        option.push(linker);
        compiler.arg("-C").arg(option);
    }
    let output = compiler
        .arg("--crate-name")
        .arg(name)
        .arg("--edition=2024")
        .arg("--target")
        .arg(target)
        .arg("-Copt-level=s")
        .arg("-Cpanic=abort")
        .arg("-Cstrip=symbols")
        .arg(source)
        .arg("-o")
        .arg(executable)
        .output()
        .expect("failed to invoke target Rust compiler for the DNS helper");
    assert!(
        output.status.success(),
        "DNS helper compilation for target {} failed; no host-architecture fallback is permitted:\n{}\n{}",
        target.to_string_lossy(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
