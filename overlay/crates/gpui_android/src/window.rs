//! One `ANativeWindow` + one `WgpuRenderer`.

use crate::{
    callback_slot::CallbackSlot, display::AndroidDisplay, ime::ImeMirror, jni_util, keymap,
    platform::PlatformState,
};
use android_activity::{
    InputStatus,
    input::{InputEvent, KeyAction, KeyEvent, Keycode, MotionAction, MotionEvent, ToolType},
};
use gpui::{
    AnyWindowHandle, Bounds, Capslock, Corners, DevicePixels, DispatchEventResult, Edges,
    GpuSpecs, KeyDownEvent, KeyUpEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler,
    PlatformWindow, Point, PromptButton, PromptLevel, RequestFrameOptions, Scene, ScrollDelta,
    ScrollEvent, ScrollSource, Size, TextInputConfiguration, TextInputStateChange, TouchEvent,
    TouchId, TouchPhase, WindowAppearance, WindowBackgroundAppearance, WindowBounds,
    WindowControlArea, WindowInsets, WindowParams, WindowVisibility, point, px, size,
};
use gpui_wgpu::{WgpuRenderer, WgpuSurfaceConfig, wgpu};
use ndk::{configuration::UiModeNight, native_window::NativeWindow};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};

/// Wrapper giving `NativeWindow` the `HasDisplayHandle` wgpu needs.
#[derive(Clone, Debug)]
pub(crate) struct RawWindow(pub NativeWindow);

impl HasWindowHandle for RawWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.0.window_handle()
    }
}

impl HasDisplayHandle for RawWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::android())
    }
}

pub(crate) fn density_scale() -> f32 {
    match crate::android_app().config().density() {
        // 0 = unknown, 0xFFFE/0xFFFF = "none"/"any" sentinels.
        Some(dpi) if dpi > 0 && dpi < 0xFFFE => dpi as f32 / 160.0,
        _ => 1.0,
    }
}

pub(crate) struct WindowState {
    platform: Weak<PlatformState>,
    native_window: RefCell<Option<RawWindow>>,
    renderer: RefCell<Option<WgpuRenderer>>,
    surface_ready: Cell<bool>,
    size_px: Cell<(i32, i32)>,
    scale: Cell<f32>,
    refresh_hz: f64,
    mouse_position: Cell<Point<Pixels>>,
    active: Cell<bool>,
    visibility: Cell<WindowVisibility>,
    force_next_frame: Cell<bool>,
    back_enabled: Cell<bool>,
    ime: RefCell<ImeMirror>,
    ime_dirty: Cell<bool>,
    ime_has_handler: Cell<bool>,
    input_handler: CallbackSlot<PlatformInputHandler>,
    request_frame_callback: CallbackSlot<Box<dyn FnMut(RequestFrameOptions)>>,
    input_callback: CallbackSlot<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    active_status_callback: CallbackSlot<Box<dyn FnMut(bool)>>,
    visibility_callback: CallbackSlot<Box<dyn FnMut(WindowVisibility)>>,
    hover_status_callback: CallbackSlot<Box<dyn FnMut(bool)>>,
    resize_callback: CallbackSlot<Box<dyn FnMut(Size<Pixels>, f32)>>,
    display_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
    moved_callback: CallbackSlot<Box<dyn FnMut()>>,
    should_close_callback: CallbackSlot<Box<dyn FnMut() -> bool>>,
    hit_test_callback: CallbackSlot<Box<dyn FnMut() -> Option<WindowControlArea>>>,
    close_callback: CallbackSlot<Box<dyn FnOnce()>>,
    appearance_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
    insets_changed_callback: CallbackSlot<Box<dyn FnMut(WindowInsets)>>,
    back_callback: CallbackSlot<Box<dyn FnMut()>>,
}

pub(crate) struct AndroidWindow(pub(crate) Rc<WindowState>);

impl std::ops::Deref for AndroidWindow {
    type Target = WindowState;
    fn deref(&self) -> &WindowState {
        &self.0
    }
}

fn surface_config(size_px: (i32, i32)) -> WgpuSurfaceConfig {
    WgpuSurfaceConfig {
        size: size(DevicePixels(size_px.0.max(1)), DevicePixels(size_px.1.max(1))),
        transparent: false,
        // Mobile: avoid blocking in get_current_texture during lifecycle changes.
        preferred_present_mode: Some(wgpu::PresentMode::Mailbox),
    }
}

impl AndroidWindow {
    pub fn new(
        _handle: AnyWindowHandle,
        _params: WindowParams,
        platform: &Rc<PlatformState>,
    ) -> anyhow::Result<Self> {
        let native = platform
            .native_window
            .borrow()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no native window yet (InitWindow not received)"))?;
        let size_px = (native.width(), native.height());
        let raw = RawWindow(native);
        let renderer = WgpuRenderer::new(
            platform.gpu_context.clone(),
            &raw,
            surface_config(size_px),
            None,
        )?;
        let state = Rc::new(WindowState {
            platform: Rc::downgrade(platform),
            native_window: RefCell::new(Some(raw)),
            renderer: RefCell::new(Some(renderer)),
            surface_ready: Cell::new(true),
            size_px: Cell::new(size_px),
            scale: Cell::new(density_scale()),
            refresh_hz: platform.refresh_hz,
            mouse_position: Cell::new(Point::default()),
            active: Cell::new(true),
            visibility: Cell::new(WindowVisibility::Visible),
            force_next_frame: Cell::new(true),
            back_enabled: Cell::new(false),
            ime: RefCell::new(ImeMirror::default()),
            ime_dirty: Cell::new(false),
            ime_has_handler: Cell::new(false),
            input_handler: Default::default(),
            request_frame_callback: Default::default(),
            input_callback: Default::default(),
            active_status_callback: Default::default(),
            visibility_callback: Default::default(),
            hover_status_callback: Default::default(),
            resize_callback: Default::default(),
            display_changed_callback: Default::default(),
            moved_callback: Default::default(),
            should_close_callback: Default::default(),
            hit_test_callback: Default::default(),
            close_callback: Default::default(),
            appearance_changed_callback: Default::default(),
            insets_changed_callback: Default::default(),
            back_callback: Default::default(),
        });
        platform.window.replace(Some(Rc::downgrade(&state)));
        Ok(Self(state))
    }
}

impl Drop for AndroidWindow {
    fn drop(&mut self) {
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            renderer.destroy();
        }
        self.close_callback.take().map(|callback| callback());
        if let Some(platform) = self.platform.upgrade() {
            platform.window.replace(None);
        }
    }
}

impl WindowState {
    fn bounds_logical(&self) -> Bounds<Pixels> {
        let (w, h) = self.size_px.get();
        let scale = self.scale.get();
        Bounds { origin: Point::default(), size: size(px(w as f32 / scale), px(h as f32 / scale)) }
    }

    // ---- lifecycle, driven by the platform event loop ----

    pub(crate) fn surface_created(&self, native: NativeWindow) {
        let size_px = (native.width(), native.height());
        self.size_px.set(size_px);
        let raw = RawWindow(native);
        let platform = self.platform.upgrade();
        let instance = platform
            .as_ref()
            .and_then(|p| p.gpu_context.borrow().as_ref().map(|c| c.instance.clone()));
        if let (Some(renderer), Some(instance)) = (self.renderer.borrow_mut().as_mut(), instance) {
            match renderer.replace_surface(&raw, surface_config(size_px), &instance) {
                Ok(()) => self.surface_ready.set(true),
                Err(error) => log::error!("failed to recreate surface: {error:#}"),
            }
        }
        self.native_window.replace(Some(raw));
        self.set_visibility(WindowVisibility::Visible);
        self.force_next_frame.set(true);
        self.notify_resize();
    }

    pub(crate) fn surface_destroyed(&self) {
        self.surface_ready.set(false);
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            renderer.unconfigure_surface();
        }
        self.native_window.replace(None);
        self.set_visibility(WindowVisibility::Hidden);
    }

    pub(crate) fn surface_resized(&self) {
        let native = self.native_window.borrow().clone();
        let Some(native) = native else { return };
        let size_px = (native.0.width(), native.0.height());
        self.scale.set(density_scale());
        if size_px != self.size_px.get() {
            self.size_px.set(size_px);
            if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
                renderer.update_drawable_size(size(DevicePixels(size_px.0.max(1)), DevicePixels(size_px.1.max(1))));
            }
        }
        self.force_next_frame.set(true);
        self.notify_resize();
    }

    fn notify_resize(&self) {
        let size = self.bounds_logical().size;
        let scale = self.scale.get();
        self.resize_callback.with(|callback| callback(size, scale));
        self.notify_insets_changed();
    }

    pub(crate) fn notify_insets_changed(&self) {
        let insets = self.current_insets();
        self.insets_changed_callback.with(|callback| callback(insets));
    }

    fn current_insets(&self) -> WindowInsets {
        // android-activity exposes the content rect (area not covered by system
        // bars/cutouts/IME), so the difference to the window is the inset.
        let rect = crate::android_app().content_rect();
        let (w, h) = self.size_px.get();
        let scale = self.scale.get();
        let safe_area = if rect.right > rect.left && rect.bottom > rect.top {
            Edges {
                top: px(rect.top.max(0) as f32 / scale),
                left: px(rect.left.max(0) as f32 / scale),
                right: px((w - rect.right).max(0) as f32 / scale),
                bottom: px((h - rect.bottom).max(0) as f32 / scale),
            }
        } else {
            Edges::default()
        };
        WindowInsets { safe_area, ime: Edges::default(), corner_radii: Corners::default() }
    }

    pub(crate) fn set_active(&self, active: bool) {
        if self.active.replace(active) != active {
            self.active_status_callback.with(|callback| callback(active));
        }
    }

    pub(crate) fn set_visibility(&self, visibility: WindowVisibility) {
        if self.visibility.replace(visibility) != visibility {
            self.visibility_callback.with(|callback| callback(visibility));
        }
    }

    pub(crate) fn appearance_changed(&self) {
        self.appearance_changed_callback.with(|callback| callback());
        self.force_next_frame.set(true);
    }

    /// Called once per display refresh by the platform loop.
    pub(crate) fn request_frame(&self) {
        if !self.surface_ready.get() || self.visibility.get() != WindowVisibility::Visible {
            return;
        }
        let force_render = self.force_next_frame.replace(false);
        self.request_frame_callback.with(|callback| {
            callback(RequestFrameOptions { force_render, ..Default::default() })
        });
    }

    pub(crate) fn refresh_hz(&self) -> f64 {
        self.refresh_hz
    }

    // ---- soft keyboard ----

    /// Reconciles keyboard visibility and the mirrored text. Called after each
    /// loop iteration so a take/set handler pair within one frame doesn't flicker.
    pub(crate) fn sync_ime(&self) {
        let has_handler = self.input_handler.is_set();
        if has_handler != self.ime_has_handler.replace(has_handler) {
            if has_handler {
                crate::android_app().show_soft_input(true);
            } else {
                crate::android_app().hide_soft_input(false);
            }
            self.ime_dirty.set(has_handler);
        }
        if !self.ime_dirty.replace(false) || !has_handler {
            return;
        }
        let state = self
            .input_handler
            .with(|handler| self.ime.borrow_mut().snapshot(handler))
            .flatten();
        if let Some(state) = state {
            crate::android_app().set_text_input_state(state);
        }
    }

    // ---- input ----

    pub(crate) fn handle_input_event(&self, event: &InputEvent<'_>) -> InputStatus {
        match event {
            InputEvent::MotionEvent(motion) => self.handle_motion(motion),
            InputEvent::KeyEvent(key) => self.handle_key(key),
            InputEvent::TextEvent(state) => {
                let handled = self
                    .input_handler
                    .with(|handler| self.ime.borrow_mut().apply(handler, state))
                    .is_some();
                self.ime_dirty.set(true);
                if handled { InputStatus::Handled } else { InputStatus::Unhandled }
            }
            _ => InputStatus::Unhandled,
        }
    }

    fn dispatch(&self, input: PlatformInput) -> DispatchEventResult {
        self.input_callback
            .with(|callback| callback(input))
            .unwrap_or_default()
    }

    fn handle_key(&self, event: &KeyEvent<'_>) -> InputStatus {
        let code = event.key_code();
        if code == Keycode::Back {
            if event.action() == KeyAction::Up && self.back_enabled.get() {
                self.back_callback.with(|callback| callback());
                return InputStatus::Handled;
            }
            return if self.back_enabled.get() { InputStatus::Handled } else { InputStatus::Unhandled };
        }
        let Some(keystroke) = keymap::keystroke(event) else {
            return InputStatus::Unhandled;
        };
        let result = match event.action() {
            KeyAction::Down => self.dispatch(PlatformInput::KeyDown(KeyDownEvent {
                keystroke,
                is_held: event.repeat_count() > 0,
                prefer_character_input: false,
            })),
            KeyAction::Up => self.dispatch(PlatformInput::KeyUp(KeyUpEvent { keystroke })),
            _ => return InputStatus::Unhandled,
        };
        if result.propagate { InputStatus::Unhandled } else { InputStatus::Handled }
    }

    fn handle_motion(&self, event: &MotionEvent<'_>) -> InputStatus {
        let scale = self.scale.get();
        let position = |x: f32, y: f32| point(px(x / scale), px(y / scale));
        let action = event.action();
        let modifiers = keymap::modifiers(event.meta_state());

        // Mouse / trackpad pointers.
        let first = event.pointer_at_index(event.pointer_index());
        if first.tool_type() == ToolType::Mouse {
            let pos = position(first.x(), first.y());
            self.mouse_position.set(pos);
            let input = match action {
                MotionAction::Down | MotionAction::ButtonPress => PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: pos,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }),
                MotionAction::Up | MotionAction::ButtonRelease => PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: pos,
                    modifiers,
                    click_count: 1,
                }),
                MotionAction::Move | MotionAction::HoverMove => PlatformInput::MouseMove(MouseMoveEvent {
                    position: pos,
                    pressed_button: event.button_state().primary().then_some(MouseButton::Left),
                    modifiers,
                }),
                MotionAction::Scroll => {
                    use android_activity::input::Axis;
                    let v = first.axis_value(Axis::Vscroll);
                    let h = first.axis_value(Axis::Hscroll);
                    PlatformInput::Scroll(ScrollEvent {
                        position: pos,
                        delta: ScrollDelta::Lines(point(h, v)),
                        modifiers,
                        touch_phase: TouchPhase::Moved,
                        source: ScrollSource::Pointer,
                    })
                }
                _ => return InputStatus::Unhandled,
            };
            let result = self.dispatch(input);
            return if result.propagate { InputStatus::Unhandled } else { InputStatus::Handled };
        }

        // Touch pointers.
        let send = |index: usize, phase: TouchPhase| {
            let pointer = event.pointer_at_index(index);
            let pos = position(pointer.x(), pointer.y());
            self.mouse_position.set(pos);
            self.dispatch(PlatformInput::Touch(TouchEvent {
                id: TouchId(pointer.pointer_id() as u64),
                phase,
                position: pos,
                predicted_position: None,
                force: Some(pointer.pressure().clamp(0.0, 1.0)),
            }));
        };
        match action {
            MotionAction::Down | MotionAction::PointerDown => send(event.pointer_index(), TouchPhase::Started),
            MotionAction::Up | MotionAction::PointerUp => send(event.pointer_index(), TouchPhase::Ended),
            MotionAction::Move => {
                for index in 0..event.pointer_count() {
                    send(index, TouchPhase::Moved);
                }
            }
            MotionAction::Cancel => {
                for index in 0..event.pointer_count() {
                    send(index, TouchPhase::Cancelled);
                }
            }
            _ => return InputStatus::Unhandled,
        }
        InputStatus::Handled
    }
}

impl HasWindowHandle for AndroidWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        match self.native_window.borrow().as_ref() {
            Some(window) => {
                let raw = window.0.window_handle()?.as_raw();
                // The ANativeWindow outlives this borrow while the surface exists.
                Ok(unsafe { WindowHandle::borrow_raw(raw) })
            }
            None => Err(HandleError::Unavailable),
        }
    }
}

impl HasDisplayHandle for AndroidWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::android())
    }
}

impl PlatformWindow for AndroidWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.bounds_logical()
    }
    fn is_maximized(&self) -> bool {
        true
    }
    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.bounds_logical())
    }
    fn content_size(&self) -> Size<Pixels> {
        self.bounds_logical().size
    }
    fn resize(&mut self, _size: Size<Pixels>) {}
    fn scale_factor(&self) -> f32 {
        self.scale.get()
    }
    fn appearance(&self) -> WindowAppearance {
        match crate::android_app().config().ui_mode_night() {
            UiModeNight::Yes => WindowAppearance::Dark,
            _ => WindowAppearance::Light,
        }
    }
    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        let bounds = self.bounds_logical();
        Some(Rc::new(AndroidDisplay::new(
            bounds.size.width.into(),
            bounds.size.height.into(),
            self.refresh_hz,
        )))
    }
    fn mouse_position(&self) -> Point<Pixels> {
        self.mouse_position.get()
    }
    fn modifiers(&self) -> Modifiers {
        Modifiers::default()
    }
    fn capslock(&self) -> Capslock {
        Capslock { on: false }
    }
    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.input_handler.set(input_handler);
        self.ime_dirty.set(true);
    }
    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.ime_dirty.set(true);
        self.input_handler.take()
    }
    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        None
    }
    fn activate(&self) {}
    fn is_active(&self) -> bool {
        self.active.get()
    }
    fn visibility(&self) -> WindowVisibility {
        self.visibility.get()
    }
    fn is_hovered(&self) -> bool {
        false
    }
    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }
    fn set_title(&mut self, _title: &str) {}
    fn set_background_appearance(&self, _background_appearance: WindowBackgroundAppearance) {}
    fn minimize(&self) {}
    fn zoom(&self) {}
    fn toggle_fullscreen(&self) {}
    fn is_fullscreen(&self) -> bool {
        true
    }
    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.request_frame_callback.set(callback);
    }
    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.input_callback.set(callback);
    }
    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.active_status_callback.set(callback);
    }
    fn on_visibility_change(&self, callback: Box<dyn FnMut(WindowVisibility)>) {
        self.visibility_callback.set(callback);
    }
    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.hover_status_callback.set(callback);
    }
    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.resize_callback.set(callback);
    }
    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.moved_callback.set(callback);
    }
    fn on_display_changed(&self, callback: Box<dyn FnMut()>) {
        self.display_changed_callback.set(callback);
    }
    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.should_close_callback.set(callback);
    }
    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.hit_test_callback.set(callback);
    }
    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.close_callback.set(callback);
    }
    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.appearance_changed_callback.set(callback);
    }
    fn draw(&self, scene: &Scene) {
        if !self.surface_ready.get() {
            return;
        }
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            renderer.draw(scene);
        }
    }
    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.renderer
            .borrow()
            .as_ref()
            .expect("renderer exists for the window's lifetime")
            .sprite_atlas()
            .clone()
    }
    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }
    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.renderer.borrow().as_ref().and_then(|r| r.gpu_specs())
    }
    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {}

    fn insets(&self) -> WindowInsets {
        self.current_insets()
    }
    fn on_insets_changed(&self, callback: Box<dyn FnMut(WindowInsets)>) {
        self.insets_changed_callback.set(callback);
    }
    fn set_back_handler(&self, callback: Box<dyn FnMut()>) {
        self.back_callback.set(callback);
    }
    fn set_back_enabled(&self, enabled: bool) {
        self.back_enabled.set(enabled);
    }
    fn show_soft_keyboard(&self) {
        crate::android_app().show_soft_input(true);
    }
    fn hide_soft_keyboard(&self) {
        crate::android_app().hide_soft_input(false);
    }
    fn text_input_state_changed(&self, _change: TextInputStateChange) {
        self.ime_dirty.set(true);
    }
    fn set_text_input_configuration(&mut self, _configuration: TextInputConfiguration) {
        // TODO: map input_action to the IME action key (needs EditorInfo via JNI).
    }
}

#[allow(dead_code)]
fn _keep(_: fn() -> f64) {
    let _ = jni_util::refresh_rate_hz;
}
