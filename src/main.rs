mod builds;
mod junk;
mod known;
mod tree;
mod ui;

use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub fn shown(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn human(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

// main shows what takes the space in home, or in the folder given, and changes
// nothing on its own. -c removes what
// nobody needs; -a also removes every build cache, which only makes the next builds slow.
fn main() {
    let home = PathBuf::from(std::env::var("HOME").expect("HOME is not set"));
    let report = |line: String| println!("{line}");
    let mut base = home.clone();
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("-c" | "--clean") => {
            junk::clean(&home, report);
            builds::prune(&home, &report);
            return;
        }
        Some("-a" | "--all") => {
            junk::clean(&home, report);
            builds::remove_all(&home, &report);
            return;
        }
        Some(arg) if !arg.starts_with('-') => match fs::canonicalize(arg) {
            Ok(path) if path.is_dir() => base = path,
            _ => {
                eprintln!("space: {arg} is not a folder");
                std::process::exit(2);
            }
        },
        Some(_) => {
            eprintln!("usage: space [folder] | -c | --clean | -a | --all");
            std::process::exit(2);
        }
    }
    if !std::io::stdout().is_terminal() {
        eprintln!("space: needs a terminal; -c and -a work without one");
        std::process::exit(1);
    }
    if let Err(err) = ui::run(home, base) {
        eprintln!("space: {err}");
        std::process::exit(1);
    }
}
