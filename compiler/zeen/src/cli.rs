#![allow(unused)]

use clap::Parser;
use colored::Colorize;
use std::{fmt::Display, path::PathBuf};

use zeen_driver::{CompilationMode, CompilationOutput};

/// Command Line Interface (CLI) for compiler
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Compiler for Zeen Programming Language",
    long_about = None,
    help_template = "{options}"
)]
pub struct Args {
    /// Path to source code
    #[arg(required_unless_present = "targets_list")]
    pub path: Option<PathBuf>,
    /// Path to output file
    #[arg(required_unless_present = "targets_list")]
    pub output: Option<PathBuf>,

    /// `--no-warns` flag to disable compiler's warnings
    #[arg(long = "no-warns", action, help = "Disable compiler's warnings")]
    pub no_warns: bool,

    /// `-m --mode` flag to specify compilation mode
    #[arg(short, long, value_enum, default_value_t = CompilationMode::Debug, help = "Specify compilation mode")]
    pub mode: CompilationMode,

    /// `--emit` emit options (BIN/OBJ/IR)
    #[arg(long, value_enum, default_value_t = CompilationOutput::Binary, help = "Emit options")]
    pub emit: CompilationOutput,

    /// `--target` flag to specify the compilation target triple
    #[arg(
        long = "target",
        value_name = "TRIPLE",
        help = "Compilation target triple (see --targets-list)"
    )]
    pub target: Option<String>,

    /// `--std` flag to specify the std library root directory. Overrides the
    /// `ZEEN_STD` environment variable and the default `~/.zeen/std` location.
    #[arg(
        long = "std",
        value_name = "PATH",
        help = "Path to the std library root (default: $ZEEN_STD or ~/.zeen/std)"
    )]
    pub std: Option<PathBuf>,

    /// `--targets-list` flag to print all supported target triples
    #[arg(long = "targets-list", action, help = "List supported target triples")]
    pub targets_list: bool,
}

pub fn println_error(message: impl Display) {
    eprintln!("{} {}", "[Error]:".red().bold(), message);
}

pub fn println_warn(message: impl Display) {
    eprintln!("{} {}", "[Warn]:".yellow().bold(), message);
}

pub fn println_info(prefix: impl Display, message: impl Display) {
    eprintln!("{} {}", format!("{}", prefix).blue().bold(), message);
}

pub fn println_primary(message: impl Display) {
    println!("{}", message.to_string().bold().blue());
}

pub fn println_basic(message: impl Display) {
    println!("{}", message);
}
