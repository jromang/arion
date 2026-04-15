use std::path::PathBuf;

fn main() {
    let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/liquid-dsp");
    println!("cargo:rerun-if-changed=vendor/liquid-dsp");

    let mut build = cc::Build::new();
    build
        .include(vendor.join("include"))
        .include(vendor.join("scripts"))
        .flag_if_supported("-std=c99")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-sign-compare")
        .flag_if_supported("-Wno-deprecated-non-prototype")
        .define("HAVE_CONFIG_H", None);

    // F.1.0 smoke build: compile only libliquid.c (error handling + version).
    // Subsequent phases expand the source list per feature (modem, filter, ...).
    build.file(vendor.join("src/libliquid.c"));

    build.compile("liquid");

    println!("cargo:rustc-link-lib=m");
}
