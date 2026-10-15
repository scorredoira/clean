mod browsers;
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

const USAGE: &str = "usage: clean -s [folder] | -b | -k | -c | -a

  -s, --space [folder]  show what takes the space in home, or in the folder
  -b, --browsers        list the Chromes bro and peek left open
  -k, --kill            close those Chromes
  -c, --clean           remove what nobody needs, those Chromes included
  -a, --all             like -c, and also every build cache";

// main changes nothing without a flag: it prints the usage.
fn main() {
    let home = PathBuf::from(std::env::var("HOME").expect("HOME is not set"));
    let report = |line: String| println!("{line}");
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-s" | "--space") if args.len() <= 2 => space(home, args.get(1)),
        Some("-b" | "--browsers") => browsers::list(report),
        Some("-k" | "--kill") => report(browsers::kill().unwrap_or_else(|| "No bro/peek browsers open".to_string())),
        Some("-c" | "--clean") => {
            junk::clean(&home, report);
            builds::prune(&home, &report);
        }
        Some("-a" | "--all") => {
            junk::clean(&home, report);
            builds::remove_all(&home, &report);
        }
        None | Some("-h" | "--help") => println!("{USAGE}"),
        Some(_) => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn space(home: PathBuf, folder: Option<&String>) {
    let base = match folder {
        None => home.clone(),
        Some(arg) => match fs::canonicalize(arg) {
            Ok(path) if path.is_dir() => path,
            _ => {
                eprintln!("clean: {arg} is not a folder");
                std::process::exit(2);
            }
        },
    };
    if !std::io::stdout().is_terminal() {
        eprintln!("clean: -s needs a terminal");
        std::process::exit(1);
    }
    if let Err(err) = ui::run(home, base) {
        eprintln!("clean: {err}");
        std::process::exit(1);
    }
}
