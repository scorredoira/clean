use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::human;

// clean removes only what nobody needs, and reports what it did.
pub fn clean(home: &Path, report: impl Fn(String)) {
    if let Some(line) = kill_debug_browsers() {
        report(line);
    }
    if let Some(line) = old_device_support(home) {
        report(line);
    }
    if let Some(line) = run(&["xcrun", "simctl", "delete", "unavailable"]) {
        report(line);
    }
    if let Some(line) = run(&["brew", "cleanup", "--prune=all"]) {
        report(line);
    }
}

// kill_debug_browsers ends every Chrome started with --remote-debugging-port, which is how
// bro and peek launch theirs; the user's own Chrome never carries that flag.
// Killing the main process takes its helpers with it.
fn kill_debug_browsers() -> Option<String> {
    let output = Command::new("ps").args(["-axo", "pid=,ppid=,rss=,command="]).output().ok()?;
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
        return None;
    }

    let memory_kb: u64 = processes
        .iter()
        .filter(|(pid, ppid, _, _)| browsers.contains(pid) || browsers.contains(ppid))
        .map(|(_, _, rss_kb, _)| rss_kb)
        .sum();
    let pids: Vec<String> = browsers.iter().map(|pid| pid.to_string()).collect();
    match Command::new("kill").args(&pids).status() {
        Ok(status) if status.success() => {
            Some(format!("Killed {} bro/peek browsers ({} of memory)", browsers.len(), human(memory_kb * 1024)))
        }
        _ => Some(format!("kill {} failed", pids.join(" "))),
    }
}

// old_device_support deletes the symbols of iOS versions not connected in 90 days;
// Xcode copies them again when such a device connects.
fn old_device_support(home: &Path) -> Option<String> {
    let root = home.join("Library/Developer/Xcode/iOS DeviceSupport");
    let limit = SystemTime::now() - Duration::from_secs(90 * 24 * 3600);
    let mut freed = 0;
    for entry in fs::read_dir(root).ok()?.flatten() {
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if modified < limit {
            let size = disk_usage(&entry.path());
            if fs::remove_dir_all(entry.path()).is_ok() {
                freed += size;
            }
        }
    }
    (freed > 0).then(|| format!("Deleted old iOS device symbols ({})", human(freed)))
}

pub fn disk_usage(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    let own = meta.blocks() * 512;
    if !meta.is_dir() {
        return own;
    }
    let children: u64 = fs::read_dir(path)
        .map(|entries| entries.flatten().map(|entry| disk_usage(&entry.path())).sum())
        .unwrap_or(0);
    own + children
}

// run reports only failures and what Homebrew says it freed; a missing tool is skipped.
fn run(command: &[&str]) -> Option<String> {
    let output = Command::new(command[0]).args(&command[1..]).output().ok()?;
    if !output.status.success() {
        return Some(format!("{} failed: {}", command.join(" "), String::from_utf8_lossy(&output.stderr).trim()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let freed = stdout.lines().find_map(|line| line.split("freed approximately ").nth(1))?;
    Some(format!("Homebrew freed {}", freed.trim_end_matches(" of disk space.")))
}
