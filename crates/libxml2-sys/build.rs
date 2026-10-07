//! Builds the vendored libxml2 (`vendor/libxml2`, a git submodule) as a static library, or with
//! `WASHBOARD_LIBXML2=pkg-config` links a system/Homebrew copy instead (development only).
//!
//! Everything we do not need is compiled out. In particular there is no network code (libxml2
//! 2.15 removed its HTTP client; FTP went earlier), no catalogs (they would let a system catalog
//! redirect schema lookups), no compression and no external libraries at all: the only link
//! dependencies are libc and libm, so the static archive behaves the same on every machine.

use std::env;
use std::path::PathBuf;

/// The oldest system libxml2 the wrapper works with: `xmlSchemaSetResourceLoader`,
/// `xmlCtxtSetResourceLoader` and `xmlNewInputFromMemory` appeared in 2.14.0.
const MIN_SYSTEM_VERSION: &str = "2.14.0";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=WASHBOARD_LIBXML2");

    let mode = env::var("WASHBOARD_LIBXML2").unwrap_or_default();
    match mode.as_str() {
        "" | "vendored" => vendored(),
        "pkg-config" => system(),
        other => panic!("WASHBOARD_LIBXML2={other:?}: expected `pkg-config` or unset"),
    }
}

fn system() {
    if let Err(e) = pkg_config::Config::new()
        .atleast_version(MIN_SYSTEM_VERSION)
        .statik(false)
        .probe("libxml-2.0")
    {
        panic!("WASHBOARD_LIBXML2=pkg-config: libxml2 >= {MIN_SYSTEM_VERSION} not found: {e}");
    }
}

fn vendored() {
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
        panic!(
            "vendor/libxml2 is missing; run `git submodule update --init` \
             (or set WASHBOARD_LIBXML2=pkg-config for a local dev build)"
        );
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
        // On: what XSD validation needs, plus push/reader for future streaming validation.
        // Without iconv, ISO-8859-x tables cover the common legacy encodings.
        .define("LIBXML2_WITH_ISO8859X", "ON")
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
