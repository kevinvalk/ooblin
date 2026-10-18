fn main() {
    // Declaring our commands makes them deny-by-default, the webview only gets what `capabilities/` grants.
    let manifest = tauri_build::AppManifest::new().commands(&["connect", "trust"]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest)).expect("tauri-build succeeds");
}
