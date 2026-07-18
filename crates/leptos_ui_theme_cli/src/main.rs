#![forbid(unsafe_code)]

use clap::Parser;
use leptos_ui_theme_cli::{Cli, run};

fn main() {
    std::process::exit(run(Cli::parse()));
}
