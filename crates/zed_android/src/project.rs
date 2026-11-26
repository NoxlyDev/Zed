//! Project directory handling.

use std::path::{Path, PathBuf};

/// Shared-storage location readable by both this app and Termux.
pub const SHARED_ROOT: &str = "/storage/emulated/0/ZedAndroid";

const HELLO_MAIN: &str = "fn main() {\n    println!(\"Hello from Rust on Android!\");\n}\n";

fn hello_manifest() -> &'static str {
    "[package]\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n"
}

/// Returns the project directory, creating a starter project if needed.
/// Falls back to the app's private storage when shared storage is not accessible.
/// The bool is true when the directory is reachable from Termux.
pub fn ensure_project() -> (PathBuf, bool) {
    let shared = Path::new(SHARED_ROOT).join("hello");
    if create_starter(&shared).is_ok() {
        return (shared, true);
    }
    let private = gpui_android::android_app()
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp"))
        .join("hello");
    let _ = create_starter(&private);
    (private, false)
}

fn create_starter(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("src"))?;
    let manifest = dir.join("Cargo.toml");
    if !manifest.exists() {
        std::fs::write(manifest, hello_manifest())?;
    }
    let main = dir.join("src/main.rs");
    if !main.exists() {
        std::fs::write(main, HELLO_MAIN)?;
    }
    Ok(())
}

/// Lists editable project files (depth-limited, skipping build output).
pub fn list_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" {
                continue;
            }
            if path.is_dir() {
                if depth < 4 {
                    walk(&path, depth + 1, out);
                }
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("rs" | "toml" | "md" | "txt" | "json" | "lock" | "yml" | "yaml" | "sh")
            ) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out
}
