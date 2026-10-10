//! Builds the vendored libxml2 as a static library.
//!
//! Everything we do not need is compiled out. In particular there is no network code, no
//! catalogs (they would let a system catalog redirect schema lookups), no compression.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let src = manifest.join("../../vendor/libxml2");
    println!("cargo:rerun-if-changed={}", src.join("NEWS").display());
    println!(
        "cargo:rerun-if-changed={}",
        src.join("CMakeLists.txt").display()
    );

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let host = env::var("HOST").unwrap_or_default();
    // `cargo clippy -p washboard-app --target aarch64-apple-darwin` on Linux runs this script
    // for a macOS target. There is no Apple SDK to compile against, and type-checking needs no
    // native library, so skip the C build; an actual link would fail, as it must without a Mac.
    if target_os == "macos" && !host.contains("apple-darwin") {
        println!(
            "cargo:warning=libxml2-sys: target macOS on a non-macOS host; C build skipped, \
             result is type-check-only (linking will fail)"
        );
        println!("cargo:rustc-link-lib=static=xml2");
        return;
    }

    if !src.join("CMakeLists.txt").exists() {
        panic!("vendor/libxml2 is missing; run `git submodule update --init`");
    }

    let mut cfg = cmake::Config::new(&src);
    cfg.define("BUILD_SHARED_LIBS", "OFF")
        .define("CMAKE_INSTALL_LIBDIR", "lib")
        .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
        // Off: network, external libraries, anything that can reach outside the bundle.
        .define("LIBXML2_WITH_HTTP", "OFF")
        .define("LIBXML2_WITH_CATALOG", "OFF")
        .define("LIBXML2_WITH_ICONV", "OFF")
        .define("LIBXML2_WITH_ICU", "OFF")
        .define("LIBXML2_WITH_ZLIB", "OFF")
        .define("LIBXML2_WITH_LEGACY", "OFF")
        .define("LIBXML2_WITH_PYTHON", "OFF")
        .define("LIBXML2_WITH_READLINE", "OFF")
        .define("LIBXML2_WITH_HISTORY", "OFF")
        .define("LIBXML2_WITH_MODULES", "OFF")
        .define("LIBXML2_WITH_THREAD_ALLOC", "OFF")
        // Off: features we do not use, to keep the attack surface and build time small.
        .define("LIBXML2_WITH_DEBUG", "OFF")
        .define("LIBXML2_WITH_DOCS", "OFF")
        .define("LIBXML2_WITH_HTML", "OFF")
        .define("LIBXML2_WITH_C14N", "OFF")
        .define("LIBXML2_WITH_XINCLUDE", "OFF")
        .define("LIBXML2_WITH_XPTR", "OFF")
        .define("LIBXML2_WITH_RELAXNG", "OFF")
        .define("LIBXML2_WITH_SCHEMATRON", "OFF")
        .define("LIBXML2_WITH_PROGRAMS", "OFF")
        .define("LIBXML2_WITH_TESTS", "OFF")
        // Off: libxml2 only ever gets UTF-8 (schemas with a BOM, instances parsed as UTF-8);
        // decoding is `xml::decode`'s job. Built-in UTF-8/16, Latin-1, ASCII and windows-1252
        // remain.
        .define("LIBXML2_WITH_ISO8859X", "OFF")
        // On: what XSD validation needs, plus push/reader for future streaming validation.
        .define("LIBXML2_WITH_SCHEMAS", "ON")
        .define("LIBXML2_WITH_PATTERN", "ON")
        .define("LIBXML2_WITH_REGEXPS", "ON")
        .define("LIBXML2_WITH_PUSH", "ON")
        .define("LIBXML2_WITH_READER", "ON")
        .define("LIBXML2_WITH_THREADS", "ON")
        .define("LIBXML2_WITH_VALID", "ON")
        .define("LIBXML2_WITH_XPATH", "ON")
        .define("LIBXML2_WITH_OUTPUT", "ON")
        .define("LIBXML2_WITH_SAX1", "ON");
    // Match Rust's deployment target so the linker does not warn about newer object files.
    if target_os == "macos" {
        let min = env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "11.0".into());
        cfg.define("CMAKE_OSX_DEPLOYMENT_TARGET", min);
    }
    let dst = cfg.build();

    println!(
        "cargo:rustc-link-search=native={}",
        dst.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=xml2");
    if target_os == "linux" {
        println!("cargo:rustc-link-lib=m");
    }
    println!("cargo:include={}", dst.join("include/libxml2").display());
}
