//! Android backend for GPUI.
//!
//! The app crate (a `cdylib`) exports `android_main`, calls [`set_android_app`],
//! then `gpui_platform::application().run(...)`. `Platform::run` pumps
//! `AndroidApp::poll_events`, input, main-thread tasks, and frames.

#![cfg(target_os = "android")]

mod callback_slot;
mod dispatcher;
mod display;
mod ime;
pub mod jni_util;
mod keymap;
mod platform;
mod window;

pub use platform::AndroidPlatform;

use android_activity::AndroidApp;
use gpui::Platform;
use std::{rc::Rc, sync::OnceLock};

static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

/// Must be called from `android_main` before creating the platform.
pub fn set_android_app(app: AndroidApp) {
    let _ = ANDROID_APP.set(app);
}

pub fn android_app() -> &'static AndroidApp {
    ANDROID_APP
        .get()
        .expect("gpui_android::set_android_app must be called from android_main first")
}

pub fn current_platform(_headless: bool) -> Rc<dyn Platform> {
    Rc::new(AndroidPlatform::new())
}
