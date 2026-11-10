//! PhotoCraft for Android.
//!
//! The same [`photocraft_ui_egui::PhotocraftApp`] as the desktop and web apps, in a
//! `NativeActivity`. Only the host differs: Open and Save go through Android's system file picker
//! (`jni_bridge`; the Java side is `android/java/.../FileBridge.java`) and reach the app the way the
//! web build's picks do, through [`photocraft_ui_egui::Services`]' file dialog and write hooks. The
//! S Pen's pressure, tilt and eraser feed the app's stylus input, and its side button acts as Shift.
//! There is no tray, window chrome, printer, or control server.
//!
//! The crate is empty on other targets so a whole-workspace build on a desktop still works.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg(target_os = "android")]

mod jni_bridge;
mod shell;

use android_activity::AndroidApp;

/// Where Android starts the app (`android.app.lib_name` in the manifest names this library).
#[allow(unsafe_code)] // `android_main` must be exported by name for android-activity's glue.
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("PhotoCraft"));
    log::info!("PhotoCraft {} starting", env!("CARGO_PKG_VERSION"));
    if let Err(e) = shell::run(app) {
        log::error!("PhotoCraft stopped: {e}");
    }
}
