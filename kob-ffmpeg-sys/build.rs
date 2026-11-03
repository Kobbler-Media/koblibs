use std::env;
use std::path::PathBuf;

const FFMPEG_LIBS: &[&str] = &[
    "libavformat",
    "libavcodec",
    "libavutil",
    "libavfilter",
    "libswresample",
    "libswscale",
];

const OPTIONAL_LIBS: &[&str] = &["libchromaprint"];

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=FFMPEG_LIB_DIR");
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=ffi_helpers.h");
    println!("cargo:rerun-if-changed=ffi_helpers.c");

    let mut include_paths = Vec::new();

    // Try pkg-config first for each library
    for lib_name in FFMPEG_LIBS {
        match pkg_config::Config::new()
            .cargo_metadata(true)
            .probe(lib_name)
        {
            Ok(lib) => {
                for path in &lib.include_paths {
                    if !include_paths.contains(path) {
                        include_paths.push(path.clone());
                    }
                }

                // Emit version cfg flags
                let short_name = lib_name.strip_prefix("lib").unwrap_or(lib_name);
                let major = lib.version.split('.').next().unwrap_or("0");
                println!("cargo:rustc-cfg={short_name}_version_{major}");
                println!(
                    "cargo:rustc-env={}_VERSION={}",
                    short_name.to_uppercase(),
                    lib.version
                );
            }
            Err(e) => {
                // Fall back to manual paths if pkg-config fails
                if let Ok(lib_dir) = env::var("FFMPEG_LIB_DIR") {
                    println!("cargo:rustc-link-search=native={lib_dir}");
                    let link_name = lib_name.strip_prefix("lib").unwrap_or(lib_name);
                    println!("cargo:rustc-link-lib={link_name}");
                } else {
                    panic!(
                        "Failed to find {lib_name} via pkg-config: {e}. \
                         Set FFMPEG_LIB_DIR and FFMPEG_INCLUDE_DIR environment variables, \
                         or install FFmpeg development headers."
                    );
                }
            }
        }
    }

    // Link chromaprint (required for audio fingerprinting)
    for lib_name in OPTIONAL_LIBS {
        match pkg_config::Config::new()
            .cargo_metadata(true)
            .probe(lib_name)
        {
            Ok(lib) => {
                for path in &lib.include_paths {
                    if !include_paths.contains(path) {
                        include_paths.push(path.clone());
                    }
                }
            }
            Err(e) => {
                panic!(
                    "Failed to find {lib_name} via pkg-config: {e}. \
                     Install libchromaprint-dev or build chromaprint from source."
                );
            }
        }
    }

    // Allow manual include path override
    if let Ok(include_dir) = env::var("FFMPEG_INCLUDE_DIR") {
        include_paths.clear();
        include_paths.push(PathBuf::from(include_dir));
    }

    // Generate bindings with bindgen
    let mut builder = bindgen::Builder::default()
        .generate_comments(true)
        .layout_tests(false) // Skip layout tests — they break across platforms
        .default_enum_style(bindgen::EnumVariation::Consts)
        .prepend_enum_name(false)
        .derive_default(true)
        // Block C standard library symbols that conflict when pulled in by multiple headers
        .blocklist_item("FP_NAN")
        .blocklist_item("FP_INFINITE")
        .blocklist_item("FP_ZERO")
        .blocklist_item("FP_SUBNORMAL")
        .blocklist_item("FP_NORMAL")
        // string.h and stdlib.h come in behind the FFmpeg headers and bring the
        // runtime's own mem* and allocator with them. They are not FFmpeg API, and
        // redeclaring them puts a second signature on symbols the compiler emits calls
        // to itself — bindgen types the length as c_ulong (or an opaque size_t) where
        // the runtime uses usize, which rustc reports as
        // `suspicious_runtime_symbol_definitions` and `-D warnings` then makes fatal.
        .blocklist_function("memcpy")
        .blocklist_function("memmove")
        .blocklist_function("memset")
        .blocklist_function("memcmp")
        .blocklist_function("bcmp")
        .blocklist_function("strlen")
        .blocklist_function("malloc")
        .blocklist_function("calloc")
        .blocklist_function("realloc")
        .blocklist_function("free")
        .header("wrapper.h");

    for path in &include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }

    let bindings = builder
        .generate()
        .expect("Failed to generate FFmpeg bindings");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Failed to write bindings");

    // Compile accessor functions for opaque FFmpeg structs
    let mut cc_build = cc::Build::new();
    cc_build.file("ffi_helpers.c");
    for path in &include_paths {
        cc_build.include(path);
    }
    cc_build.compile("kob_ffmpeg_helpers");
}
