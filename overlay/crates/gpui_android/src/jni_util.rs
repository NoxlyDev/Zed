//! Small JNI helpers for things GPUI needs from the Android framework.
//! Public so the app crate can reuse them (e.g. for the Termux bridge).

use anyhow::{Result, anyhow};
use jni::{
    JNIEnv, JavaVM,
    objects::{JObject, JString, JValue},
};

/// Runs `f` on an attached JNI env with the current Activity as context.
/// Pending Java exceptions are logged, cleared, and turned into errors.
pub fn with_env<R>(
    f: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<R>,
) -> Result<R> {
    let app = crate::android_app();
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) }
        .map_err(|e| anyhow!("JavaVM::from_raw: {e}"))?;
    let mut env = vm.attach_current_thread().map_err(|e| anyhow!("attach: {e}"))?;
    // The activity reference is owned by android-activity; JObject has no Drop.
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
    let result = f(&mut env, &activity);
    if env.exception_check().unwrap_or(false) {
        env.exception_describe().ok();
        env.exception_clear().ok();
        return Err(anyhow!("a Java exception was thrown"));
    }
    result.map_err(|e| anyhow!("JNI error: {e}"))
}

pub fn write_clipboard(text: &str) -> Result<()> {
    with_env(|env, activity| {
        let service = env.new_string("clipboard")?;
        let manager = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service)],
            )?
            .l()?;
        let label = env.new_string("gpui")?;
        let text = env.new_string(text)?;
        let clip = env
            .call_static_method(
                "android/content/ClipData",
                "newPlainText",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                &[JValue::Object(&label), JValue::Object(&text)],
            )?
            .l()?;
        env.call_method(
            &manager,
            "setPrimaryClip",
            "(Landroid/content/ClipData;)V",
            &[JValue::Object(&clip)],
        )?;
        Ok(())
    })
}

pub fn read_clipboard() -> Result<Option<String>> {
    with_env(|env, activity| {
        let service = env.new_string("clipboard")?;
        let manager = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service)],
            )?
            .l()?;
        let clip = env
            .call_method(&manager, "getPrimaryClip", "()Landroid/content/ClipData;", &[])?
            .l()?;
        if clip.is_null() {
            return Ok(None);
        }
        let count = env.call_method(&clip, "getItemCount", "()I", &[])?.i()?;
        if count < 1 {
            return Ok(None);
        }
        let item = env
            .call_method(&clip, "getItemAt", "(I)Landroid/content/ClipData$Item;", &[JValue::Int(0)])?
            .l()?;
        let text = env
            .call_method(
                &item,
                "coerceToText",
                "(Landroid/content/Context;)Ljava/lang/CharSequence;",
                &[JValue::Object(activity)],
            )?
            .l()?;
        if text.is_null() {
            return Ok(None);
        }
        let string = env
            .call_method(&text, "toString", "()Ljava/lang/String;", &[])?
            .l()?;
        let string: String = env.get_string(&JString::from(string))?.into();
        Ok(Some(string))
    })
}

pub fn open_url(url: &str) -> Result<()> {
    with_env(|env, activity| {
        let url = env.new_string(url)?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&url)],
            )?
            .l()?;
        let action = env.new_string("android.intent.action.VIEW")?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    })
}

pub fn finish_activity() -> Result<()> {
    with_env(|env, activity| {
        env.call_method(activity, "finish", "()V", &[])?;
        Ok(())
    })
}

/// Directory holding the app's bundled native libraries.
pub fn native_library_dir() -> Result<String> {
    with_env(|env, activity| {
        let info = env
            .call_method(activity, "getApplicationInfo", "()Landroid/content/pm/ApplicationInfo;", &[])?
            .l()?;
        let dir = env
            .get_field(&info, "nativeLibraryDir", "Ljava/lang/String;")?
            .l()?;
        Ok(env.get_string(&JString::from(dir))?.into())
    })
}

/// Display refresh rate in Hz (falls back to 60 on failure).
pub fn refresh_rate_hz() -> f64 {
    with_env(|env, activity| {
        let wm = env.call_method(activity, "getWindowManager", "()Landroid/view/WindowManager;", &[])?.l()?;
        let display = env.call_method(&wm, "getDefaultDisplay", "()Landroid/view/Display;", &[])?.l()?;
        Ok(env.call_method(&display, "getRefreshRate", "()F", &[])?.f()? as f64)
    })
    .unwrap_or(60.0)
}
