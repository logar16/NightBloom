// Tauri's resource/config generation exists only for the feature-gated desktop executable, so
// the headless server and core tests build without it.
fn main() {
    #[cfg(feature = "desktop")]
    tauri_build::build();
}
