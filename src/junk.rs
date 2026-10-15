use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::human;

// clean removes only what nobody needs, and reports what it did.
pub fn clean(home: &Path, report: impl Fn(String)) {
    if let Some(line) = crate::browsers::kill() {
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
