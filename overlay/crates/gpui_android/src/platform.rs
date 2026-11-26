//! `Platform` for Android.

use crate::{
    android_app, callback_slot::CallbackSlot, dispatcher::AndroidDispatcher,
    display::AndroidDisplay, jni_util, window::{AndroidWindow, WindowState, density_scale},
};
use android_activity::{MainEvent, PollEvent};
use anyhow::{Result, anyhow};
use futures::channel::oneshot;
use gpui::{
    Action, AnyWindowHandle, AppLifecyclePhase, BackgroundExecutor, ClipboardItem, CursorStyle,
    DummyKeyboardMapper, ForegroundExecutor, Keymap, Menu, MenuItem, PathPromptOptions, Platform,
    PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, PowerMode, PriorityQueueReceiver, RunnableVariant, Task, ThermalState,
    WindowAppearance, WindowParams,
};
use gpui_wgpu::{CosmicTextSystem, GpuContext};
use ndk::{configuration::UiModeNight, native_window::NativeWindow};
use parking_lot::Mutex;
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) struct PlatformState {
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<dyn PlatformTextSystem>,
    main_receiver: Mutex<PriorityQueueReceiver<RunnableVariant>>,
    pub(crate) gpu_context: GpuContext,
    pub(crate) native_window: RefCell<Option<NativeWindow>>,
    pub(crate) window: RefCell<Option<Weak<WindowState>>>,
    pub(crate) refresh_hz: f64,
    window_handle: Cell<Option<AnyWindowHandle>>,
    quit_requested: Cell<bool>,
    quit_callback: CallbackSlot<Box<dyn FnMut()>>,
    lifecycle_callback: CallbackSlot<Box<dyn FnMut(AppLifecyclePhase)>>,
    memory_callback: CallbackSlot<Box<dyn FnMut()>>,
    open_urls_callback: CallbackSlot<Box<dyn FnMut(Vec<String>)>>,
    displays_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
}

pub struct AndroidPlatform(Rc<PlatformState>);

struct AndroidKeyboardLayout;
impl PlatformKeyboardLayout for AndroidKeyboardLayout {
    fn id(&self) -> &str {
        "android-default"
    }
    fn name(&self) -> &str {
        "Android Default"
    }
}

impl Default for AndroidPlatform {
    fn default() -> Self {
        Self::new()
    }
}

/// Loads the system fonts GPUI needs that cosmic-text may not discover itself.
fn load_system_fonts(text_system: &CosmicTextSystem) {
    use std::borrow::Cow;
    let candidates = [
        "/system/fonts/Roboto-Regular.ttf",
        "/system/fonts/RobotoStatic-Regular.ttf",
        "/system/fonts/DroidSansMono.ttf",
        "/system/fonts/NotoSansMono-Regular.ttf",
        "/system/fonts/NotoColorEmoji.ttf",
    ];
    let fonts: Vec<Cow<'static, [u8]>> = candidates
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .map(Cow::Owned)
        .collect();
    if !fonts.is_empty() {
        if let Err(error) = gpui::PlatformTextSystem::add_fonts(text_system, fonts) {
            log::warn!("failed to load Android system fonts: {error:#}");
        }
    }
}

impl AndroidPlatform {
    pub fn new() -> Self {
        let (dispatcher, main_receiver) = AndroidDispatcher::new(android_app().create_waker());
        let dispatcher = Arc::new(dispatcher);
        let text_system = CosmicTextSystem::new("Roboto");
        load_system_fonts(&text_system);
        Self(Rc::new(PlatformState {
            background_executor: BackgroundExecutor::new(dispatcher.clone()),
            foreground_executor: ForegroundExecutor::new(dispatcher),
            text_system: Arc::new(text_system),
            main_receiver: Mutex::new(main_receiver),
            gpu_context: Rc::new(RefCell::new(None)),
            native_window: RefCell::new(None),
            window: RefCell::new(None),
            refresh_hz: jni_util::refresh_rate_hz(),
            window_handle: Cell::new(None),
            quit_requested: Cell::new(false),
            quit_callback: Default::default(),
            lifecycle_callback: Default::default(),
            memory_callback: Default::default(),
            open_urls_callback: Default::default(),
            displays_changed_callback: Default::default(),
        }))
    }
}

impl PlatformState {
    fn window(&self) -> Option<Rc<WindowState>> {
        self.window.borrow().as_ref().and_then(Weak::upgrade)
    }

    fn lifecycle(&self, phase: AppLifecyclePhase) {
        self.lifecycle_callback.with(|callback| callback(phase));
    }

    fn handle_main_event(&self, event: MainEvent<'_>) {
        match event {
            MainEvent::InitWindow { .. } => {
                let native = android_app().native_window();
                self.native_window.replace(native.clone());
                if let (Some(window), Some(native)) = (self.window(), native) {
                    window.surface_created(native);
                }
            }
            MainEvent::TerminateWindow { .. } => {
                if let Some(window) = self.window() {
                    window.surface_destroyed();
                }
                self.native_window.replace(None);
            }
            MainEvent::WindowResized { .. } | MainEvent::RedrawNeeded { .. } => {
                if let Some(window) = self.window() {
                    window.surface_resized();
                }
            }
            MainEvent::ContentRectChanged { .. } | MainEvent::InsetsChanged { .. } => {
                if let Some(window) = self.window() {
                    window.notify_insets_changed();
                }
            }
            MainEvent::ConfigChanged { .. } => {
                if let Some(window) = self.window() {
                    window.appearance_changed();
                    window.surface_resized();
                }
            }
            MainEvent::GainedFocus => {
                if let Some(window) = self.window() {
                    window.set_active(true);
                }
            }
            MainEvent::LostFocus => {
                if let Some(window) = self.window() {
                    window.set_active(false);
                }
            }
            MainEvent::Resume { .. } => self.lifecycle(AppLifecyclePhase::Active),
            MainEvent::Pause => self.lifecycle(AppLifecyclePhase::Inactive),
            MainEvent::Stop => self.lifecycle(AppLifecyclePhase::Background),
            MainEvent::Start => self.lifecycle(AppLifecyclePhase::Foreground),
            MainEvent::LowMemory => {
                self.memory_callback.with(|callback| callback());
            }
            MainEvent::Destroy => self.quit_requested.set(true),
            _ => {}
        }
    }

    fn drain_input(&self) {
        let Some(window) = self.window() else {
            // Still drain so Android doesn't keep waking us for input.
            if let Ok(mut iter) = android_app().input_events_iter() {
                while iter.next(|_| android_activity::InputStatus::Unhandled) {}
            }
            return;
        };
        if let Ok(mut iter) = android_app().input_events_iter() {
            while iter.next(|event| window.handle_input_event(event)) {}
        }
    }

    fn drain_main_tasks(&self) {
        let mut receiver = self.main_receiver.lock();
        // Bounded so a task that keeps spawning cannot starve input and frames.
        for _ in 0..256 {
            match receiver.try_pop() {
                Ok(Some(runnable)) => {
                    drop(receiver);
                    runnable.run();
                    receiver = self.main_receiver.lock();
                }
                _ => break,
            }
        }
    }

    fn run_loop(self: &Rc<Self>, on_finish_launching: Box<dyn FnOnce()>) {
        let app = android_app().clone();
        let mut on_finish_launching = Some(on_finish_launching);
        let mut next_frame = Instant::now();

        while !self.quit_requested.get() {
            // Until the first surface exists there is nothing to draw: block.
            let timeout = if self.native_window.borrow().is_none() {
                Some(Duration::from_millis(250))
            } else {
                Some(next_frame.saturating_duration_since(Instant::now()))
            };
            app.poll_events(timeout, |event| {
                if let PollEvent::Main(main) = event {
                    self.handle_main_event(main);
                }
            });

            // GPUI opens its window in on_finish_launching, which needs a surface.
            if self.native_window.borrow().is_some() {
                if let Some(callback) = on_finish_launching.take() {
                    callback();
                }
            }

            self.drain_input();
            self.drain_main_tasks();

            if let Some(window) = self.window() {
                let interval = Duration::from_secs_f64(1.0 / window.refresh_hz().max(30.0));
                if Instant::now() >= next_frame {
                    window.request_frame();
                    next_frame = Instant::now() + interval;
                }
                window.sync_ime();
            }
        }
        self.quit_callback.with(|callback| callback());
    }
}

impl Platform for AndroidPlatform {
    fn platform_info(&self) -> gpui::PlatformInfo {
        gpui::PlatformInfo {
            os: gpui::OperatingSystem::Android,
            target: gpui::PlatformTarget::Native,
        }
    }
    fn background_executor(&self) -> BackgroundExecutor {
        self.0.background_executor.clone()
    }
    fn foreground_executor(&self) -> ForegroundExecutor {
        self.0.foreground_executor.clone()
    }
    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.0.text_system.clone()
    }
    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        self.0.run_loop(on_finish_launching);
    }
    fn quit(&self) {
        self.0.quit_requested.set(true);
        if let Err(error) = jni_util::finish_activity() {
            log::warn!("finish(): {error:#}");
        }
    }
    fn restart(&self, _binary_path: Option<PathBuf>, _arguments: Vec<std::ffi::OsString>) {
        log::warn!("Android apps cannot restart themselves");
    }
    fn activate(&self, _ignoring_other_apps: bool) {}
    fn hide(&self) {}
    fn hide_other_apps(&self) {}
    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        self.primary_display().into_iter().collect()
    }
    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        let (w, h) = self.0.native_window.borrow().as_ref().map(|n| (n.width(), n.height()))?;
        let scale = density_scale();
        Some(Rc::new(AndroidDisplay::new(w as f32 / scale, h as f32 / scale, self.0.refresh_hz)))
    }
    fn on_displays_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.displays_changed_callback.set(callback);
    }
    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.0.window_handle.get()
    }
    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        if self.0.window().is_some() {
            return Err(anyhow!("Android supports a single GPUI window"));
        }
        let window = AndroidWindow::new(handle, options, &self.0)?;
        self.0.window_handle.set(Some(handle));
        Ok(Box::new(window))
    }
    fn window_appearance(&self) -> WindowAppearance {
        match android_app().config().ui_mode_night() {
            UiModeNight::Yes => WindowAppearance::Dark,
            _ => WindowAppearance::Light,
        }
    }

    fn open_url(&self, url: &str) {
        if let Err(error) = jni_util::open_url(url) {
            log::error!("open_url({url}): {error:#}");
        }
    }
    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.0.open_urls_callback.set(callback);
    }
    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        // TODO(phase 3): Storage Access Framework. Until then projects live in
        // app-accessible directories (e.g. Termux home via the bridge).
        let (tx, rx) = oneshot::channel();
        tx.send(Err(anyhow!("File picker is not implemented on Android yet"))).ok();
        rx
    }
    fn prompt_for_new_path(
        &self,
        _directory: &Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Err(anyhow!("Save dialog is not implemented on Android yet"))).ok();
        rx
    }
    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }
    fn reveal_path(&self, _path: &Path) {}
    fn open_with_system(&self, _path: &Path) {}

    fn on_quit(&self, mut callback: Box<dyn FnMut() -> bool>) {
        self.0.quit_callback.set(Box::new(move || {
            callback();
        }));
    }
    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {}
    fn on_system_sleep(&self, _callback: Box<dyn FnMut()>) {}
    fn on_system_wake(&self, _callback: Box<dyn FnMut()>) {}
    fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) {
        self.0.lifecycle_callback.set(callback);
    }
    fn on_memory_warning(&self, callback: Box<dyn FnMut()>) {
        self.0.memory_callback.set(callback);
    }

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {}
    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}
    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {}
    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}
    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {}

    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }
    fn on_thermal_state_change(&self, _callback: Box<dyn FnMut()>) {}
    fn power_mode(&self) -> PowerMode {
        PowerMode::Unknown
    }
    fn prevent_idle_sleep(&self, _reason: &str) -> Task<Result<gpui::ActivityGuard>> {
        Task::ready(Err(anyhow!("Idle sleep prevention is not implemented on Android")))
    }

    fn app_path(&self) -> Result<PathBuf> {
        Ok(PathBuf::from(jni_util::native_library_dir()?))
    }
    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        // Executables ship as lib<name>.so in the native library dir.
        Ok(self.app_path()?.join(format!("lib{name}.so")))
    }

    fn set_cursor_style(&self, _style: CursorStyle) {}
    fn hide_cursor_until_mouse_moves(&self) {}
    fn is_cursor_visible(&self) -> bool {
        false
    }
    fn should_auto_hide_scrollbars(&self) -> bool {
        true
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        jni_util::read_clipboard()
            .map_err(|error| log::warn!("read clipboard: {error:#}"))
            .ok()
            .flatten()
            .map(ClipboardItem::new_string)
    }
    fn write_to_clipboard(&self, item: ClipboardItem) {
        if let Some(text) = item.text() {
            if let Err(error) = jni_util::write_clipboard(&text) {
                log::warn!("write clipboard: {error:#}");
            }
        }
    }

    fn write_credentials(&self, _url: &str, _username: &str, _password: &[u8]) -> Task<Result<()>> {
        // TODO: Android Keystore-backed storage.
        Task::ready(Err(anyhow!("credential storage is not implemented on Android")))
    }
    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }
    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(AndroidKeyboardLayout)
    }
    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(DummyKeyboardMapper)
    }
    fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {}
}
