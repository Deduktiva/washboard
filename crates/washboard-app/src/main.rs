//! Washboard macOS app. All of it lives in the library, so `tests/appkit.rs` can build the
//! same objects headless; this only starts the run loop.

#[cfg(target_os = "macos")]
fn main() {
    let mtm = objc2::MainThreadMarker::new().expect("main() runs on the main thread");
    washboard_app::run(mtm);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("washboard-app runs on macOS only");
    std::process::exit(1);
}
