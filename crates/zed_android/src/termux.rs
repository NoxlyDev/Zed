//! Bridge to Termux (Phase 4).
//!
//! Zed-Android asks Termux to run a shell script through the documented
//! `com.termux.RUN_COMMAND` intent. The script connects back to a loopback TCP
//! listener owned by this app (authenticated by a random token) and streams the
//! command's output, so build output appears live in the output panel.
//!
//! Required one-time setup in Termux:
//!   echo "allow-external-apps=true" >> ~/.termux/termux.properties
//!   termux-setup-storage        # lets Termux read/write /sdcard projects
//!   pkg install rust            # toolchain used for Build/Run

use anyhow::{Context as _, Result, anyhow};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui_android::jni_util::with_env;
use jni::objects::{JObject, JValue};
use std::{
    io::{BufRead, BufReader, Read},
    net::{TcpListener, TcpStream},
    thread,
};

pub const TERMUX_PACKAGE: &str = "com.termux";
const TERMUX_BASH: &str = "/data/data/com.termux/files/usr/bin/bash";
const TERMUX_HOME: &str = "/data/data/com.termux/files/home";
pub const RUN_COMMAND_PERMISSION: &str = "com.termux.permission.RUN_COMMAND";

#[derive(Debug, Clone)]
pub enum BridgeEvent {
    Line(String),
    Closed,
}

pub struct Bridge {
    pub port: u16,
    pub token: String,
}

fn random_token() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        file.read_exact(&mut bytes).ok();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Bridge {
    /// Starts the loopback listener. Events arrive on the returned receiver.
    pub fn start() -> Result<(Self, UnboundedReceiver<BridgeEvent>)> {
        let listener = TcpListener::bind("127.0.0.1:0").context("binding loopback listener")?;
        let port = listener.local_addr()?.port();
        let token = random_token();
        let (tx, rx) = unbounded();
        let expected = token.clone();
        thread::Builder::new().name("termux-bridge".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                let expected = expected.clone();
                thread::spawn(move || serve(stream, &expected, tx));
            }
        })?;
        Ok((Self { port, token }, rx))
    }

    /// Runs `command` inside `workdir` in Termux, streaming output back.
    pub fn run(&self, workdir: &str, command: &str) -> Result<()> {
        let quoted = format!("'{}'", workdir.replace('\'', "'\\''"));
        let script = format!(
            "exec 3<>/dev/tcp/127.0.0.1/{port}\n\
             echo {token} >&3\n\
             export TERM=dumb CARGO_TERM_COLOR=never\n\
             export CARGO_TARGET_DIR=\"$HOME/.cache/zed-android/target\"\n\
             cd {quoted} || {{ echo \"cannot cd to {quoted}\" >&3; exit 1; }}\n\
             {command} >&3 2>&1\n\
             echo \"[exit code: $?]\" >&3\n\
             exec 3>&-\n",
            port = self.port,
            token = self.token,
        );
        start_termux_command(&script)
    }
}

fn serve(stream: TcpStream, token: &str, tx: UnboundedSender<BridgeEvent>) {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() || first.trim() != token {
        return; // wrong or missing token: drop the connection
    }
    for line in reader.lines() {
        match line {
            Ok(line) => {
                if tx.unbounded_send(BridgeEvent::Line(line)).is_err() {
                    return;
                }
            }
            Err(_) => break,
        }
    }
    tx.unbounded_send(BridgeEvent::Closed).ok();
}

fn string_array<'a>(
    env: &mut jni::JNIEnv<'a>,
    items: &[&str],
) -> jni::errors::Result<jni::objects::JObjectArray<'a>> {
    let class = env.find_class("java/lang/String")?;
    let array = env.new_object_array(items.len() as i32, class, JObject::null())?;
    for (i, item) in items.iter().enumerate() {
        let s = env.new_string(item)?;
        env.set_object_array_element(&array, i as i32, s)?;
    }
    Ok(array)
}

fn start_termux_command(script: &str) -> Result<()> {
    let result = with_env(|env, activity| {
        let intent = env.new_object("android/content/Intent", "()V", &[])?;
        let pkg = env.new_string(TERMUX_PACKAGE)?;
        let class = env.new_string("com.termux.app.RunCommandService")?;
        env.call_method(
            &intent,
            "setClassName",
            "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&pkg), JValue::Object(&class)],
        )?;
        let action = env.new_string("com.termux.RUN_COMMAND")?;
        env.call_method(&intent, "setAction", "(Ljava/lang/String;)Landroid/content/Intent;", &[JValue::Object(&action)])?;

        let put_string = |env: &mut jni::JNIEnv<'_>, key: &str, value: &str| -> jni::errors::Result<()> {
            let k = env.new_string(key)?;
            let v = env.new_string(value)?;
            env.call_method(
                &intent,
                "putExtra",
                "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
                &[JValue::Object(&k), JValue::Object(&v)],
            )?;
            Ok(())
        };
        put_string(env, "com.termux.RUN_COMMAND_PATH", TERMUX_BASH)?;
        put_string(env, "com.termux.RUN_COMMAND_WORKDIR", TERMUX_HOME)?;

        let args = string_array(env, &["-lc", script])?;
        let k = env.new_string("com.termux.RUN_COMMAND_ARGUMENTS")?;
        env.call_method(
            &intent,
            "putExtra",
            "(Ljava/lang/String;[Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&k), JValue::Object(&args)],
        )?;
        let k = env.new_string("com.termux.RUN_COMMAND_BACKGROUND")?;
        env.call_method(
            &intent,
            "putExtra",
            "(Ljava/lang/String;Z)Landroid/content/Intent;",
            &[JValue::Object(&k), JValue::Bool(1)],
        )?;

        let component = env
            .call_method(
                activity,
                "startForegroundService",
                "(Landroid/content/Intent;)Landroid/content/ComponentName;",
                &[JValue::Object(&intent)],
            )?
            .l()?;
        Ok(component.is_null())
    })?;
    if result {
        Err(anyhow!(
            "Termux was not found or refused the command. Install Termux (F-Droid/GitHub), set \
             allow-external-apps=true in ~/.termux/termux.properties, and grant the RUN_COMMAND permission."
        ))
    } else {
        Ok(())
    }
}

/// Asks Android to show the runtime permission dialog for Termux commands.
pub fn request_run_command_permission() -> Result<()> {
    with_env(|env, activity| {
        let perms = string_array(env, &[RUN_COMMAND_PERMISSION])?;
        env.call_method(
            activity,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[JValue::Object(&perms), JValue::Int(1001)],
        )?;
        Ok(())
    })
}

pub fn has_run_command_permission() -> bool {
    with_env(|env, activity| {
        let perm = env.new_string(RUN_COMMAND_PERMISSION)?;
        let granted = env
            .call_method(activity, "checkSelfPermission", "(Ljava/lang/String;)I", &[JValue::Object(&perm)])?
            .i()?;
        Ok(granted == 0)
    })
    .unwrap_or(false)
}

/// Whether the app may read/write shared storage (needed for /sdcard projects).
#[allow(dead_code)]
pub fn has_all_files_access() -> bool {
    with_env(|env, _| {
        Ok(env
            .call_static_method("android/os/Environment", "isExternalStorageManager", "()Z", &[])?
            .z()?)
    })
    .unwrap_or(false)
}

/// Opens the system "All files access" settings page for this app.
pub fn request_all_files_access() -> Result<()> {
    with_env(|env, activity| {
        let action = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
        let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
        let name = env
            .call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?
            .l()?;
        let scheme = env.new_string("package")?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "fromParts",
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&scheme), JValue::Object(&name), JValue::Object(&JObject::null())],
            )?
            .l()?;
        env.call_method(&intent, "setData", "(Landroid/net/Uri;)Landroid/content/Intent;", &[JValue::Object(&uri)])?;
        env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    })
}
