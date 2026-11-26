//! Root view: toolbar, file drawer, editor, output panel, key bar.

use crate::{
    editor::{Editor, MONO_FONT},
    project,
    termux::{self, Bridge, BridgeEvent},
};
use futures::StreamExt as _;
use gpui::{prelude::*, *};
use std::path::PathBuf;

const BG: u32 = 0x0b1220;
const PANEL: u32 = 0x111827;
const BUTTON: u32 = 0x1f2937;
const BUTTON_ACTIVE: u32 = 0x374151;
const TEXT: u32 = 0xe5e7eb;
const DIM: u32 = 0x9ca3af;
const ACCENT: u32 = 0x2563eb;
const MAX_OUTPUT_LINES: usize = 2000;

const SYMBOLS: &[&str] = &[
    "{", "}", "(", ")", "[", "]", ";", ":", "\"", "'", "<", ">", "=", "-", "_", "/", "\\", "|",
    "&", "!", "#", "$", "%", "*", "+", ".", ",", "?",
];

pub struct AppView {
    editor: Entity<Editor>,
    project_dir: PathBuf,
    termux_reachable: bool,
    files: Vec<PathBuf>,
    show_files: bool,
    output: Vec<String>,
    output_scroll: ScrollHandle,
    bridge: Option<Bridge>,
    running: bool,
    status: SharedString,
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(BUTTON))
        .text_color(rgb(TEXT))
        .active(|style| style.bg(rgb(BUTTON_ACTIVE)))
        .child(label.into())
}

impl AppView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(Editor::new);
        let (project_dir, termux_reachable) = project::ensure_project();
        let files = project::list_files(&project_dir);

        let mut this = Self {
            editor,
            project_dir: project_dir.clone(),
            termux_reachable,
            files,
            show_files: false,
            output: Vec::new(),
            output_scroll: ScrollHandle::new(),
            bridge: None,
            running: false,
            status: "Ready".into(),
        };
        this.open_file(project_dir.join("src/main.rs"), cx);

        match Bridge::start() {
            Ok((bridge, mut events)) => {
                this.bridge = Some(bridge);
                cx.spawn(async move |this, cx| {
                    while let Some(event) = events.next().await {
                        let updated = this.update(cx, |this, cx| {
                            match event {
                                BridgeEvent::Line(line) => this.push_output(line),
                                BridgeEvent::Closed => {
                                    this.running = false;
                                    this.status = "Finished".into();
                                }
                            }
                            cx.notify();
                        });
                        if updated.is_err() {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => this.status = format!("Bridge unavailable: {error:#}").into(),
        }
        this
    }

    pub fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        let handle = self.editor.read(cx).focus_handle.clone();
        window.focus(&handle, cx);
    }

    fn push_output(&mut self, line: String) {
        self.output.push(line);
        if self.output.len() > MAX_OUTPUT_LINES {
            self.output.drain(..self.output.len() - MAX_OUTPUT_LINES);
        }
        self.output_scroll.scroll_to_bottom();
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.editor.update(cx, |editor, cx| editor.set_contents(text, Some(path), cx));
            }
            Err(error) => self.status = format!("Cannot open {}: {error}", path.display()).into(),
        }
        cx.notify();
    }

    fn run_command(&mut self, label: &str, command: &str, cx: &mut Context<Self>) {
        if let Err(error) = self.editor.update(cx, |editor, cx| editor.save(cx)) {
            self.status = format!("Save failed: {error}").into();
            cx.notify();
            return;
        }
        if !self.termux_reachable {
            self.status = "Grant \"All files access\", then restart, so Termux can see the project".into();
            cx.notify();
            return;
        }
        if !termux::has_run_command_permission() {
            self.status = "Allow the Termux RUN_COMMAND permission, then tap again".into();
            termux::request_run_command_permission().ok();
            cx.notify();
            return;
        }
        if self.bridge.is_none() {
            self.status = "Output bridge is not running".into();
            cx.notify();
            return;
        }
        self.output.clear();
        self.push_output(format!("$ {command}"));
        let workdir = self.project_dir.to_string_lossy().to_string();
        let result = self.bridge.as_ref().map(|bridge| bridge.run(&workdir, command));
        match result {
            Some(Ok(())) => {
                self.running = true;
                self.status = format!("{label}…").into();
            }
            Some(Err(error)) => {
                self.push_output(format!("{error:#}"));
                self.status = "Could not start Termux command".into();
            }
            None => {}
        }
        cx.notify();
    }

    fn command_button(
        &self,
        id: &'static str,
        label: &'static str,
        command: &'static str,
        primary: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        button(id, label)
            .when(primary, |b| b.bg(rgb(ACCENT)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.run_command(label, command, cx)
            }))
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let insets = window.insets();
        let title = {
            let editor = self.editor.read(cx);
            let name = editor
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".into());
            format!("{name}{}", if editor.dirty { " •" } else { "" })
        };

        let top_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .bg(rgb(PANEL))
            .child(button("files", "☰").on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.files = project::list_files(&this.project_dir);
                this.show_files = !this.show_files;
                cx.notify();
            })))
            .child(div().flex_1().truncate().text_color(rgb(TEXT)).child(title))
            .child(button("save", "Save").on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                let result = this.editor.update(cx, |e, cx| e.save(cx));
                this.status = match result {
                    Ok(()) => "Saved".into(),
                    Err(error) => format!("Save failed: {error}").into(),
                };
                cx.notify();
            })))
            .child(self.command_button("check", "Check", "cargo check --color never", false, cx))
            .child(self.command_button("build", "Build", "cargo build --color never", false, cx))
            .child(self.command_button("run", "Run ▶", "cargo run --color never", true, cx))
            .child(self.command_button("test", "Test", "cargo test --color never", false, cx));

        let drawer = self.show_files.then(|| {
            let root = self.project_dir.clone();
            div()
                .id("drawer")
                .w(px(220.))
                .flex_none()
                .h_full()
                .overflow_y_scroll()
                .bg(rgb(PANEL))
                .border_r_1()
                .border_color(rgb(BUTTON))
                .flex()
                .flex_col()
                .children(self.files.iter().enumerate().map(|(i, path)| {
                    let label = path.strip_prefix(&root).unwrap_or(path).display().to_string();
                    let path = path.clone();
                    div()
                        .id(("file", i))
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .active(|s| s.bg(rgb(BUTTON_ACTIVE)))
                        .child(label)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_file(path.clone(), cx);
                            this.show_files = false;
                            this.focus_editor(window, cx);
                        }))
                }))
        });

        let permission_banner = (!self.termux_reachable).then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .bg(rgb(0x78350f))
                .text_color(rgb(0xfef3c7))
                .text_sm()
                .child(div().flex_1().child("Projects are private to this app. Grant All files access so Termux can build them."))
                .child(button("grant", "Grant").on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                    if let Err(error) = termux::request_all_files_access() {
                        this.status = format!("{error:#}").into();
                    }
                    cx.notify();
                })))
        });

        let output = (!self.output.is_empty() || self.running).then(|| {
            div()
                .flex_none()
                .h(px(190.))
                .flex()
                .flex_col()
                .bg(rgb(BG))
                .border_t_1()
                .border_color(rgb(BUTTON))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child(div().flex_1().child(self.status.clone()))
                        .child(button("clear", "Clear").on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.output.clear();
                            cx.notify();
                        }))),
                )
                .child(
                    div()
                        .id("output")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.output_scroll)
                        .px_3()
                        .font_family(MONO_FONT)
                        .text_size(px(12.))
                        .text_color(rgb(TEXT))
                        .flex()
                        .flex_col()
                        .children(self.output.iter().map(|line| div().child(SharedString::from(line.clone())))),
                )
        });

        let key = |id: &'static str, label: &'static str, key: &'static str| {
            button(id, label).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.editor.update(cx, |editor, cx| {
                    editor.handle_key(key, false, false, window, cx);
                });
            }))
        };
        let ctrl_active = self.editor.read(cx).ctrl_sticky;
        let key_bar = div()
            .id("keybar")
            .flex_none()
            .flex()
            .flex_row()
            .gap_1()
            .px_1()
            .py_1()
            .overflow_x_scroll()
            .bg(rgb(PANEL))
            .child(key("k-tab", "Tab", "tab"))
            .child(
                button("k-ctrl", "Ctrl")
                    .when(ctrl_active, |b| b.bg(rgb(ACCENT)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.editor.update(cx, |e, cx| {
                            e.ctrl_sticky = !e.ctrl_sticky;
                            cx.notify();
                        });
                    })),
            )
            .child(key("k-left", "←", "left"))
            .child(key("k-up", "↑", "up"))
            .child(key("k-down", "↓", "down"))
            .child(key("k-right", "→", "right"))
            .child(key("k-home", "Home", "home"))
            .child(key("k-end", "End", "end"))
            .children(SYMBOLS.iter().enumerate().map(|(i, symbol)| {
                let symbol: &'static str = symbol;
                button(("sym", i), symbol).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.editor.update(cx, |editor, cx| editor.insert(symbol, window, cx));
                }))
            }));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .pt(insets.safe_area.top)
            .pb(insets.safe_area.bottom)
            .pl(insets.safe_area.left)
            .pr(insets.safe_area.right)
            .child(top_bar)
            .children(permission_banner)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .children(drawer)
                    .child(div().flex_1().min_w_0().h_full().child(self.editor.clone())),
            )
            .children(output)
            .child(key_bar)
    }
}
