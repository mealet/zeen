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
    #[arg(required_unless_present_any = ["targets_list", "check"])]
    pub output: Option<PathBuf>,

    /// `--check` flag to run all checks without codegen
    #[arg(long, action, help = "Run all checks without codegen")]
    pub check: bool,

    /// `--no-warns` flag to disable compiler's warnings
    #[arg(long = "no-warns", action, help = "Disable compiler's warnings")]
    pub no_warns: bool,

    /// `--no-color` flag to disable coloring on output
    #[arg(long = "no-color", action, help = "Disable coloring output")]
    pub no_color: bool,

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
    /// `ZEEN_STD` environment variable, the `~/.zeen/std` location, and the
    /// `share/zeen/std` directory installed next to the executable.
    #[arg(
        long = "std",
        value_name = "PATH",
        help = "Path to the std library root (default: $ZEEN_STD, ~/.zeen/std, then share/zeen/std)"
    )]
    pub std: Option<PathBuf>,

    /// `--linker-path` flag to override the detected linker executable
    #[arg(
        long = "linker-path",
        value_name = "PROGRAM",
        help = "Override the detected linker executable"
    )]
    pub linker_path: Option<PathBuf>,

    /// `--pkg` flag to map a package name to a source directory
    #[arg(
        long = "pkg",
        value_name = "NAME=PATH",
        help = "Map package name to source directory for imports"
    )]
    pub pkg: Vec<String>,

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
