#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod components;
mod folders;
mod local;
mod native;
mod shell_env;
mod updates;
mod workbench;

fn main() {
    native::run();
}
