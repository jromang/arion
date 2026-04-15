use std::path::{Path, PathBuf};

fn main() {
    let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/ft8_lib");
    println!("cargo:rerun-if-changed=vendor/ft8_lib");
    println!("cargo:rerun-if-changed=build.rs");

    let mut build = cc::Build::new();
    build
        .include(&vendor)
        .flag_if_supported("-std=c99")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-sign-compare")
        .define("HAVE_STPCPY", None)
        .define("_GNU_SOURCE", None);

    for sub in ["ft8", "common", "fft"] {
        collect_c(&vendor.join(sub), &mut build);
    }
    build.compile("ft8");
    println!("cargo:rustc-link-lib=m");
}

fn collect_c(dir: &Path, build: &mut cc::Build) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("c") {
            build.file(path);
        }
    }
}
