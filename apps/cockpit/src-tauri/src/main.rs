// No console window on Windows release builds (macOS/Linux ignore this).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    mirante_cockpit_lib::run()
}
