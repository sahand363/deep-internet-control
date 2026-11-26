//! Build script (v1.0):
//! 1. Compiles the Slint UI (`ui/app.slint`).
//! 2. On a Windows MSVC target, embeds a requireAdministrator UAC
//!    manifest through the linker, so the exe asks for admin rights
//!    on launch (WinDivert + WFP need them). This works both on a
//!    Windows build host and when cross-compiling (lld-link), unlike
//!    the old winres step that only ran on a Windows host.

fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent".into()),
    )
    .expect("Slint UI failed to compile");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:level='requireAdministrator' uiAccess='false'");
    }
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=build.rs");
}
