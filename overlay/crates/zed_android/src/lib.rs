//! Zed for Android.
//!
//! A GPUI application running on `gpui_android`: a touch-friendly code editor
//! whose Build/Run/Test buttons execute `cargo` inside Termux.

#![cfg(target_os = "android")]

mod app_view;
mod editor;
mod project;
mod termux;

use android_activity::AndroidApp;
use gpui::{App, AppContext as _, WindowOptions};
use std::borrow::Cow;

fn load_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/lilex/Lilex-Regular.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/lilex/Lilex-Bold.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf")[..]),
    ];
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        log::error!("failed to load bundled fonts: {error:#}");
    }
}

fn run() {
    gpui_platform::application().run(|cx: &mut App| {
        load_fonts(cx);
        let window = cx
            .open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| app_view::AppView::new(window, cx))
            })
            .expect("failed to open the main window");
        window
            .update(cx, |view, window, cx| {
                view.focus_editor(window, cx);
                cx.activate(true);
            })
            .ok();
    });
}

#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("zed_android"),
    );
    std::panic::set_hook(Box::new(|info| log::error!("panic: {info}")));
    gpui_android::set_android_app(app);
    run();
}
