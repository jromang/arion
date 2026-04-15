use std::path::{Path, PathBuf};

fn main() {
    let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/ft8_lib");
    println!("cargo:rerun-if-changed=vendor/ft8_lib");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=shim.c");

    let target = std::env::var("TARGET").unwrap_or_default();
    let is_windows = target.contains("windows");

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
        .flag_if_supported("-Wno-implicit-function-declaration")
        .define("_GNU_SOURCE", None);

    // stpcpy is a GNU/POSIX extension that mingw's libc doesn't
    // provide; force-include a prototype stub on Windows targets
    // and ship the implementation in shim.c. Non-Windows uses the
    // libc function directly.
    if is_windows {
        let hdr = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mingw_compat.h");
        build.flag_if_supported(&format!("-include{}", hdr.display()));
    } else {
        build.define("HAVE_STPCPY", None);
    }

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
