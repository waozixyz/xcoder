use std::path::PathBuf;
use std::process::Command;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let ziran = std::env::var_os("ZIRAN").unwrap_or_else(|| "ziran".into());
    println!("cargo:rerun-if-env-changed=ZIRAN");
    for file in [
        "native/dispatch.zi",
        "native/runtime.zi",
        "ziran.toml",
        "ziran.lock",
        "ziran.local.toml",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
    let mut command = Command::new(&ziran);
    command
        .current_dir(&root)
        .args(["build", "--project", "--target=c", "-o"])
        .arg(out.join("native"))
        .arg("native/dispatch.zi");
    if !root.join("ziran.local.toml").exists() {
        command.arg("--locked");
    }
    assert!(
        command.status().expect("run Zi2c").success(),
        "Zi2c dispatcher generation failed"
    );
    cc::Build::new()
        .file(out.join("native/dispatch.c"))
        .include(out.join("native"))
        .flag_if_supported("-std=c99")
        .opt_level(3)
        .compile("xcoder_dispatch");
    let toolchain = Command::new(&ziran)
        .current_dir(root)
        .args(["pkg", "path", "ziran"])
        .output()
        .expect("resolve Ziran toolchain");
    assert!(toolchain.status.success(), "resolve locked Ziran toolchain");
    println!(
        "cargo:rustc-env=XCODER_ZIRAN_TOOLCHAIN_DEFAULT={}",
        String::from_utf8(toolchain.stdout)
            .expect("toolchain path UTF-8")
            .trim()
    );
}
