use std::fs;
use std::io::IsTerminal;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;

enum Action {
    // Remove deletes these paths; each is regenerated when needed.
    Remove(Vec<PathBuf>),
    // Run cleans through the tool that owns the cache.
    Run(&'static [&'static str]),
    // Keep only reports: deleting it is the user's call.
    Keep(&'static str),
}

struct Item {
    name: String,
    paths: Vec<PathBuf>,
    action: Action,
}

fn disk_usage(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    let own = meta.blocks() * 512;
    if !meta.is_dir() {
        return own;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return own;
    };
    let children: u64 = entries
        .flatten()
        .map(|entry| disk_usage(&entry.path()))
        .sum();
    own + children
}

// find_dirs collects build folders under root without descending into them:
// a cargo `target` is recognized by its CACHEDIR.TAG, `node_modules` by name.
fn find_dirs(root: &Path, depth: usize, targets: &mut Vec<PathBuf>, node_modules: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" && path.join("CACHEDIR.TAG").exists() {
            targets.push(path);
        } else if name == "node_modules" {
            node_modules.push(path);
        } else if name != ".git" {
            find_dirs(&path, depth - 1, targets, node_modules);
        }
    }
}

const GREEN: &str = "32";
const YELLOW: &str = "33";
const DIM: &str = "2";

// paint colors text only on a terminal, so a pipe gets plain text.
fn paint(text: &str, color: &str) -> String {
    if std::io::stdout().is_terminal() {
        format!("\x1b[{color}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn human(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

// kill_debug_browsers ends every Chrome started with --remote-debugging-port, which is how
// bro and peek launch theirs; the user's own Chrome never carries that flag.
// Killing the main process takes its helpers with it.
fn kill_debug_browsers() {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,rss=,command="])
        .output()
        .expect("ps failed");
    let listing = String::from_utf8_lossy(&output.stdout);
    let processes: Vec<(u32, u32, u64, &str)> = listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let rss_kb: u64 = fields.next()?.parse().ok()?;
            let command_start = line.find(" /").or_else(|| line.rfind(' '))?;
            Some((pid, ppid, rss_kb, line[command_start..].trim()))
        })
        .collect();

    let browsers: Vec<u32> = processes
        .iter()
        .filter(|(_, _, _, command)| {
            let executable = command.split(" --").next().unwrap_or("");
            executable.ends_with("/Contents/MacOS/Google Chrome") && command.contains(" --remote-debugging-port")
        })
        .map(|(pid, _, _, _)| *pid)
        .collect();
    if browsers.is_empty() {
        println!("No bro/peek browsers running");
        return;
    }

    let memory_kb: u64 = processes
        .iter()
        .filter(|(pid, ppid, _, _)| browsers.contains(pid) || browsers.contains(ppid))
        .map(|(_, _, rss_kb, _)| rss_kb)
        .sum();
    let pids: Vec<String> = browsers.iter().map(|pid| pid.to_string()).collect();
    let status = Command::new("kill").args(&pids).status().expect("kill failed");
    if !status.success() {
        panic!("kill {} failed", pids.join(" "));
    }
    println!("Killed {} bro/peek browsers ({} of memory)", browsers.len(), human(memory_kb * 1024));
}

fn main() {
    kill_debug_browsers();
    println!();

    let home = PathBuf::from(std::env::var("HOME").expect("HOME is not set"));
    let at = |relative: &str| home.join(relative);

    let mut items = vec![
        Item { name: "Cargo registry".into(), paths: vec![at(".cargo/registry"), at(".cargo/git")], action: Action::Remove(vec![at(".cargo/registry/cache"), at(".cargo/registry/src"), at(".cargo/git/checkouts")]) },
        Item { name: "Go build cache".into(), paths: vec![at("Library/Caches/go-build")], action: Action::Run(&["go", "clean", "-cache"]) },
        Item { name: "Go module cache".into(), paths: vec![at("go/pkg/mod")], action: Action::Run(&["go", "clean", "-modcache"]) },
        Item { name: "staticcheck + goimports".into(), paths: vec![at("Library/Caches/staticcheck"), at("Library/Caches/goimports")], action: Action::Remove(vec![at("Library/Caches/staticcheck"), at("Library/Caches/goimports")]) },
        Item { name: "iOS simulators".into(), paths: vec![at("Library/Developer/CoreSimulator/Devices")], action: Action::Run(&["xcrun", "simctl", "delete", "unavailable"]) },
        Item { name: "iOS DeviceSupport".into(), paths: vec![at("Library/Developer/Xcode/iOS DeviceSupport")], action: Action::Remove(vec![at("Library/Developer/Xcode/iOS DeviceSupport")]) },
        Item { name: "Xcode DerivedData".into(), paths: vec![at("Library/Developer/Xcode/DerivedData")], action: Action::Remove(vec![at("Library/Developer/Xcode/DerivedData")]) },
        Item { name: "Android emulators".into(), paths: vec![at(".android/avd")], action: Action::Keep("Android Studio > Device Manager") },
        Item { name: "Android SDK".into(), paths: vec![at("Library/Android/sdk")], action: Action::Keep("Android Studio > SDK Manager") },
        Item { name: "Gradle caches".into(), paths: vec![at(".gradle/caches")], action: Action::Remove(vec![at(".gradle/caches")]) },
        Item { name: "npm cache".into(), paths: vec![at(".npm")], action: Action::Remove(vec![at(".npm/_cacache")]) },
        Item { name: "Homebrew cache".into(), paths: vec![at("Library/Caches/Homebrew")], action: Action::Run(&["brew", "cleanup", "--prune=all"]) },
        Item { name: "Trash".into(), paths: vec![at(".Trash")], action: Action::Keep("empty the Trash") },
    ];

    let mut targets = Vec::new();
    let mut node_modules = Vec::new();
    for root in ["Documents/projects", "projects"] {
        find_dirs(&at(root), 5, &mut targets, &mut node_modules);
    }
    for target in targets {
        let project = target.parent().unwrap().to_path_buf();
        let shown = project.strip_prefix(&home).unwrap_or(&project).display().to_string();
        items.push(Item {
            name: format!("Rust target ~/{shown}"),
            paths: vec![target.clone()],
            action: Action::Remove(vec![target]),
        });
    }
    items.push(Item {
        name: format!("node_modules ({} folders)", node_modules.len()),
        paths: node_modules,
        action: Action::Keep("rm -rf the ones of inactive projects"),
    });

    println!("Cleaning {} places in parallel...", items.len());
    let (sender, receiver) = mpsc::channel();
    let (freed, kept) = thread::scope(|scope| {
        for item in items {
            let sender = sender.clone();
            scope.spawn(move || {
                let line = clean(item);
                sender.send(line).unwrap();
            });
        }
        drop(sender);

        let mut freed = 0;
        let mut kept = 0;
        for (name, before, after, hint) in receiver {
            let gone = before.saturating_sub(after);
            freed += gone;
            kept += after;
            if gone >= 50_000_000 {
                println!("{}  {:>8}  {name}", paint("freed", GREEN), human(gone));
            }
            if after >= 50_000_000 {
                println!("{}  {:>8}  {name}  {}", paint("kept ", YELLOW), human(after), paint(&format!("({hint})"), DIM));
            }
        }
        (freed, kept)
    });
    println!("{}  {:>8}  total, {} kept", paint("freed", GREEN), human(freed), human(kept));
}

// clean applies the item's action and returns its name, its size before and after, and
// why what is left was left.
fn clean(item: Item) -> (String, u64, u64, String) {
    let size = |paths: &[PathBuf]| paths.iter().map(|path| disk_usage(path)).sum::<u64>();
    let before = size(&item.paths);
    let hint = match item.action {
        Action::Keep(hint) => hint.to_string(),
        Action::Remove(paths) => {
            for path in paths {
                if path.exists() {
                    if let Err(err) = fs::remove_dir_all(&path) {
                        eprintln!("cannot remove {}: {err}", path.display());
                    }
                }
            }
            "not cache".to_string()
        }
        Action::Run(command) => {
            if before > 0 {
                match Command::new(command[0]).args(&command[1..]).output() {
                    Ok(output) if !output.status.success() => {
                        eprintln!("{} failed: {}", command.join(" "), String::from_utf8_lossy(&output.stderr).trim());
                    }
                    Err(err) => eprintln!("{} failed: {err}", command.join(" ")),
                    Ok(_) => {}
                }
            }
            format!("left by {}", command.join(" "))
        }
    };
    let after = size(&item.paths);
    (item.name, before, after, hint)
}
