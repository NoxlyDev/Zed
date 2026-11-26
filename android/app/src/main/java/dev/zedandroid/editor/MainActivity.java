package dev.zedandroid.editor;

import com.google.androidgamesdk.GameActivity;

/** Hosts the Rust (GPUI) application; all UI is drawn natively. */
public class MainActivity extends GameActivity {
    static {
        System.loadLibrary("zed_android");
    }
}
