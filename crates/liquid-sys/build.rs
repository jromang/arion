use std::path::{Path, PathBuf};

fn main() {
    let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/liquid-dsp");
    println!("cargo:rerun-if-changed=vendor/liquid-dsp");
    println!("cargo:rerun-if-changed=build.rs");

    let mut build = cc::Build::new();
    build
        .include(vendor.join("include"))
        .include(vendor.join("scripts"))
        .flag_if_supported("-std=c99")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-result")
        .flag_if_supported("-Wno-sign-compare")
        .flag_if_supported("-Wno-deprecated-non-prototype")
        .flag_if_supported("-Wno-deprecated-declarations")
        .flag_if_supported("-Wno-implicit-function-declaration")
        // liquid-dsp's gasearch.c passes calloc(element_size, count)
        // (transposed vs the canonical calloc(count, element_size)).
        // Harmless — the product is the same — but modern gcc warns.
        .flag_if_supported("-Wno-calloc-transposed-args")
        .flag_if_supported("-Wno-implicit-fallthrough")
        .define("HAVE_CONFIG_H", None)
        .define("_GNU_SOURCE", None);

    build.file(vendor.join("src/libliquid.c"));
    let src_root = vendor.join("src");
    collect_portable_sources(&src_root, &mut |p| {
        build.file(p);
    });

    build.compile("liquid");
    println!("cargo:rustc-link-lib=m");
}

/// Walk `src/*/src/` and keep portable `.c` sources only:
/// skip templates (`.proto.c`, `.port.c`, `.shim.c`) and arch-specific
/// SIMD variants (`.avx.c`, `.avx512f.c`, `.sse.c`, `.neon.c`, `.av.c`).
/// liquid's build system selects one SIMD variant via autoconf;
/// we pick the portable fallback unconditionally.
fn collect_portable_sources(src_root: &Path, visit: &mut impl FnMut(PathBuf)) {
    let Ok(modules) = std::fs::read_dir(src_root) else {
        return;
    };
    for module in modules.flatten() {
        let module_src = module.path().join("src");
        let Ok(entries) = std::fs::read_dir(&module_src) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("c") {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.ends_with(".proto.c")
                || name.ends_with(".port.c")
                || name.ends_with(".shim.c")
                || name.ends_with(".avx.c")
                || name.ends_with(".avx512f.c")
                || name.ends_with(".sse.c")
                || name.ends_with(".neon.c")
                || name.ends_with(".av.c")
            {
                continue;
            }
            visit(path);
        }
    }
}
