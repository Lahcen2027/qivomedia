fn main() {
    // Declaring the app's commands makes Tauri generate one permission per command
    // (`allow-device-info`, …). Nothing is callable unless a capability grants it, and
    // the server's pages only get the player commands (see `grant_server_access`).
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
        "device_info",
        "screenshot",
        "heartbeat",
        "reboot",
        "restart_player",
        "display_power",
        "open_setup",
        "get_config",
        "save_config",
        "test_server",
        "quit_player",
    ])))
    .expect("failed to run tauri-build");
}
