use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::{human, shown};
use crate::junk::disk_usage;
use crate::known::{self, Kind, Tag};

const ROOTS: [&str; 2] = ["Documents/projects", "projects"];
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(24 * 3600);

// find collects the build folders under the project roots, without descending into them.
pub fn find(home: &Path) -> Vec<(PathBuf, &'static Tag)> {
    let mut found = Vec::new();
    for root in ROOTS {
        walk(home, &home.join(root), 6, &mut found);
    }
    found
}

fn walk(home: &Path, dir: &Path, depth: usize, found: &mut Vec<(PathBuf, &'static Tag)>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) || entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        match known::find(home, &path) {
            Some(tag) if matches!(tag.kind, Kind::Rebuilds) => found.push((path, tag)),
            Some(_) => {}
            None => walk(home, &path, depth - 1, found),
        }
    }
}

// remove_all deletes every build folder in the projects and every cache in home that
// comes back by itself.
pub fn remove_all(home: &Path, report: &dyn Fn(String)) {
    let mut items = find(home);
    items.extend(known::home_rebuilds(home));
    for (path, tag) in items {
        let before = disk_usage(&path);
        if let Err(err) = known::clean(home, Some(tag), &path) {
            report(err);
        }
        let freed = before.saturating_sub(disk_usage(&path));
        if freed >= 50_000_000 {
            report(format!("Deleted {} ({})", shown(home, &path), human(freed)));
        }
    }
}

// prune deletes from each cargo build folder what no current build uses: the folders
// of agent worktrees that are gone, and in the rest the leftovers of past builds.
pub fn prune(home: &Path, report: &dyn Fn(String)) {
    let worktrees = agent_worktrees(home);
    for (path, tag) in find(home) {
        if !std::ptr::eq(tag, &known::RUST_TARGET) {
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with("agent-") && !worktrees.contains(name.as_ref()) {
            let size = disk_usage(&path);
            match known::remove(&path) {
                Ok(()) => report(format!("Deleted {}, its agent worktree is gone ({})", shown(home, &path), human(size))),
                Err(err) => report(err),
            }
            continue;
        }
        let freed = match prune_target(&path) {
            Ok(freed) => freed,
            Err(why) => {
                report(format!("{}: {why}, left as is", shown(home, &path)));
                continue;
            }
        };
        if freed >= 50_000_000 {
            report(format!("Pruned {} ({})", shown(home, &path), human(freed)));
        }
    }
}

// agent_worktrees names the Claude Code agent worktrees of every project, which build
// into a folder of the same name.
fn agent_worktrees(home: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    for root in ROOTS {
        let Ok(projects) = fs::read_dir(home.join(root)) else {
            continue;
        };
        for project in projects.flatten() {
            let Ok(worktrees) = fs::read_dir(project.path().join(".claude/worktrees")) else {
                continue;
            };
            names.extend(worktrees.flatten().map(|worktree| worktree.file_name().to_string_lossy().into_owned()));
        }
    }
    names
}

// profiles finds debug, release and the other profiles, also under a target triple.
fn profiles(target: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(target) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join(".fingerprint").is_dir() {
            found.push(path);
        } else if let Ok(inner) = fs::read_dir(&path) {
            found.extend(inner.flatten().map(|entry| entry.path()).filter(|path| path.join(".fingerprint").is_dir()));
        }
    }
    found
}

struct Unit {
    deps: Vec<u64>,
    // key is the same for every build of one target in one profile and mode.
    key: String,
    time: SystemTime,
}

// prune_target keeps the units reachable from the latest build of each target, by the
// dependencies cargo records in .fingerprint, and deletes the rest. All profiles are
// read together, as a cross build uses build scripts and macros of the host profile.
// It reads the cargo records strictly: anything it does not understand leaves the
// folder untouched.
fn prune_target(target: &Path) -> Result<u64, String> {
    let profiles = profiles(target);
    for profile in &profiles {
        if let Ok(lock) = File::open(profile.join(".cargo-lock")) {
            if lock.try_lock().is_err() {
                return Err("a build is running".into());
            }
        }
    }
    let mut units: HashMap<PathBuf, Unit> = HashMap::new();
    let mut by_hash: HashMap<u64, PathBuf> = HashMap::new();
    for profile in &profiles {
        for entry in fs::read_dir(profile.join(".fingerprint")).map_err(|err| err.to_string())?.flatten() {
            let path = entry.path();
            let time = modified(&path.join("invoked.timestamp")).or_else(|| modified(&path)).unwrap_or(SystemTime::UNIX_EPOCH);
            let unit = read_unit(&path, time, &mut by_hash)?;
            units.insert(path, unit);
        }
    }

    let mut used = HashSet::new();
    for unit in units.values() {
        for dep in &unit.deps {
            used.insert(by_hash.get(dep).ok_or("a dependency cargo does not record")?);
        }
    }
    let mut newest: HashMap<&str, SystemTime> = HashMap::new();
    for (path, unit) in &units {
        if !used.contains(path) {
            let time = newest.entry(&unit.key).or_insert(unit.time);
            *time = (*time).max(unit.time);
        }
    }
    let mut stack: Vec<&PathBuf> = units
        .iter()
        .filter(|(path, unit)| !used.contains(path) && unit.time + HOUR >= newest[unit.key.as_str()])
        .map(|(path, _)| path)
        .collect();
    let mut keep = HashSet::new();
    while let Some(path) = stack.pop() {
        if keep.insert(path) {
            stack.extend(units[path].deps.iter().map(|dep| &by_hash[dep]));
        }
    }

    let mut freed = 0;
    for profile in &profiles {
        let junk: HashSet<&str> = units
            .keys()
            .filter(|path| path.parent().and_then(Path::parent) == Some(profile.as_path()) && !keep.contains(path))
            .filter_map(|path| hash_of(path.file_name()?.to_str()?))
            .collect();
        for folder in ["deps", "build", ".fingerprint"] {
            let Ok(entries) = fs::read_dir(profile.join(folder)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if hash_of(&name).is_some_and(|hash| junk.contains(hash)) {
                    freed += delete(&entry.path());
                }
            }
        }
        freed += prune_incremental(&profile.join("incremental"));
    }
    Ok(freed)
}

fn read_unit(path: &Path, time: SystemTime, by_hash: &mut HashMap<u64, PathBuf>) -> Result<Unit, String> {
    let mut unit = Unit { deps: Vec::new(), key: String::new(), time };
    let unreadable = |file_name: &str| format!("cannot read {}/{file_name}", path.display());
    for file in fs::read_dir(path).map_err(|err| err.to_string())?.flatten() {
        let file_name = file.file_name().to_string_lossy().into_owned();
        if file_name.ends_with(".json") {
            let text = fs::read_to_string(file.path()).map_err(|err| err.to_string())?;
            let json: Value = serde_json::from_str(&text).map_err(|_| unreadable(&file_name))?;
            for dep in json["deps"].as_array().ok_or_else(|| unreadable(&file_name))? {
                unit.deps.push(dep[3].as_u64().ok_or_else(|| unreadable(&file_name))?);
            }
            unit.key.push_str(&format!("{file_name} {} {} {};", json["target"], json["profile"], json["compile_kind"]));
        } else if !file_name.contains('.') && !file_name.starts_with("dep-") && !file_name.starts_with("output-") {
            if let Some(hash) = fs::read_to_string(file.path()).ok().and_then(|text| parse_hash(text.trim())) {
                by_hash.insert(hash, path.to_path_buf());
            }
        }
    }
    Ok(unit)
}

// prune_incremental deletes the incremental state of a crate that was not compiled in
// the last two days of its newest one; rustc names it after the crate, not the unit.
fn prune_incremental(incremental: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(incremental) else {
        return 0;
    };
    let mut crates: HashMap<String, Vec<(PathBuf, SystemTime)>> = HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((krate, _)) = name.rsplit_once('-') else {
            continue;
        };
        let time = modified(&entry.path()).unwrap_or(SystemTime::UNIX_EPOCH);
        crates.entry(krate.to_string()).or_default().push((entry.path(), time));
    }
    let mut freed = 0;
    for states in crates.values() {
        let newest = states.iter().map(|(_, time)| *time).max().unwrap_or(SystemTime::UNIX_EPOCH);
        for (path, time) in states {
            if *time + 2 * DAY < newest {
                freed += delete(path);
            }
        }
    }
    freed
}

fn delete(path: &Path) -> u64 {
    let size = disk_usage(path);
    if known::remove(path).is_ok() { size } else { 0 }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

// parse_hash reads a fingerprint file, which holds the bytes of the hash as hex in
// little-endian order.
fn parse_hash(text: &str) -> Option<u64> {
    if text.len() != 16 {
        return None;
    }
    let mut bytes = [0u8; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(u64::from_le_bytes(bytes))
}

// hash_of finds the unit hash cargo puts in a file name, as in libserde-01a4d0aabcd3f6d9.rmeta.
fn hash_of(name: &str) -> Option<&str> {
    name.match_indices('-').find_map(|(index, _)| {
        let hash = name.get(index + 1..index + 17)?;
        let rest = &name[index + 17..];
        (hash.bytes().all(|byte| byte.is_ascii_hexdigit()) && (rest.is_empty() || rest.starts_with('.'))).then_some(hash)
    })
}
