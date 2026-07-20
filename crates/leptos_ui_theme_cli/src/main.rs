#![forbid(unsafe_code)]

use leptos_ui_theme_cli::run_from;

fn main() {
    std::process::exit(run_from(std::env::args_os()));
}
