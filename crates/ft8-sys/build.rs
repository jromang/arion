use std::path::{Path, PathBuf};

fn main() {
    let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/ft8_lib");
    println!("cargo:rerun-if-changed=vendor/ft8_lib");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=shim.c");

    let mut build = cc::Build::new();
    build
        .include(&vendor)
        .flag_if_supported("-std=c99")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-sign-compare")
        // ft8_lib's debug LOG() macros use %llx for uint64_t, which
        // is long long on Windows but plain long on 64-bit Linux.
        // The format mismatch only affects debug prints, which Arion
        // disables anyway; silence the noise.
        .flag_if_supported("-Wno-format")
        .flag_if_supported("-Wno-discarded-qualifiers")
        .define("HAVE_STPCPY", None)
        .define("_GNU_SOURCE", None);

    for sub in ["ft8", "common", "fft"] {
        collect_c(&vendor.join(sub), &mut build);
    }
    build.file(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shim.c"));
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
