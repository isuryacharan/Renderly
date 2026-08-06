// Release builds run under the Windows GUI subsystem so launching the app does not open a
// console window alongside it. Debug builds keep the console for `eprintln!`/panic output.
// (No-op on non-Windows.)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    renderly_app_lib::run();
}
