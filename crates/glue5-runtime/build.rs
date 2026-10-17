//! Links the glue5-runtime's native bits: the Bink movie shim, compiled
//! against the host's FFmpeg snapshot (the engine's own Bink SDK is stripped
//! from the wasm). Only Android links the decoder: the desktop checks run
//! against the null renderer, which decodes no movies.

use std::path::{Path, PathBuf};

const LIBS: &[&str] = &["avformat", "avcodec", "swresample", "avutil"];

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    let ffmpeg = std::env::var_os("GLUE5_FFMPEG")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("target")
                .join("ffmpeg-android-aarch64")
        });
    let lib = ffmpeg.join("lib");
    let include = ffmpeg.join("include");
    assert!(lib.is_dir(), "FFmpeg (the movie decoder) is missing at {}", lib.display());
    assert!(include.is_dir(), "FFmpeg's headers are missing at {}", include.display());
    println!("cargo:rerun-if-changed={}", lib.display());
    println!("cargo:rerun-if-changed=src/bink_shim.c");
    println!("cargo:rustc-link-search=native={}", lib.display());
    for name in LIBS {
        println!("cargo:rustc-link-lib=static={name}");
    }
    cc::Build::new()
        .file("src/bink_shim.c")
        .include(&include)
        .flag("-fPIC")
        .warnings(false)
        .compile("bink_shim");
}
