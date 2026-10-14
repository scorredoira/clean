use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub enum Kind {
    // Rebuilds comes back by itself on the next build, only slower.
    Rebuilds,
    // Downloads comes back, but has to be downloaded again.
    Downloads,
    // Yours is not regenerated: deleting it loses something.
    Yours,
}

pub enum Action {
    Remove,
    // Empty deletes what is inside and keeps the folder.
    Empty,
    // Run cleans through the tool that owns the folder.
    Run(&'static [&'static str]),
    // Manual is cleaned from the app that owns it.
    Manual(&'static str),
}

pub struct Tag {
    pub label: &'static str,
    pub kind: Kind,
    pub action: Action,
}

const fn tag(label: &'static str, kind: Kind, action: Action) -> Tag {
    Tag { label, kind, action }
}

static BY_PATH: &[(&str, Tag)] = &[
    (".cargo/registry", tag("Cargo registry", Kind::Downloads, Action::Remove)),
    (".cargo/git", tag("Cargo git", Kind::Downloads, Action::Remove)),
    ("Library/Caches/go-build", tag("Go build", Kind::Rebuilds, Action::Run(&["go", "clean", "-cache"]))),
    ("go/pkg/mod", tag("Go modules", Kind::Downloads, Action::Run(&["go", "clean", "-modcache"]))),
    ("Library/Caches/staticcheck", tag("staticcheck", Kind::Rebuilds, Action::Remove)),
    ("Library/Caches/goimports", tag("goimports", Kind::Rebuilds, Action::Remove)),
    ("Library/Caches/Homebrew", tag("Homebrew", Kind::Downloads, Action::Run(&["brew", "cleanup", "--prune=all"]))),
    ("Library/Developer/CoreSimulator/Devices", tag("iOS simulators", Kind::Yours, Action::Manual("Xcode > Devices and Simulators"))),
    ("Library/Developer/Xcode/iOS DeviceSupport", tag("device symbols", Kind::Rebuilds, Action::Remove)),
    ("Library/Developer/Xcode/DerivedData", tag("Xcode build", Kind::Rebuilds, Action::Remove)),
    ("Library/Developer/Xcode/Archives", tag("Xcode archives", Kind::Yours, Action::Manual("Xcode > Organizer"))),
    (".android/avd", tag("Android emulators", Kind::Yours, Action::Manual("Android Studio > Device Manager"))),
    ("Library/Android/sdk", tag("Android SDK", Kind::Downloads, Action::Manual("Android Studio > SDK Manager"))),
    (".gradle/caches", tag("Gradle", Kind::Downloads, Action::Remove)),
    (".npm/_cacache", tag("npm", Kind::Downloads, Action::Remove)),
    ("Library/Caches/ccache", tag("ccache", Kind::Rebuilds, Action::Remove)),
    (".Trash", tag("Trash", Kind::Yours, Action::Empty)),
];

pub static RUST_TARGET: Tag = tag("Rust build", Kind::Rebuilds, Action::Remove);
static BUILD_CACHE: Tag = tag("build cache", Kind::Rebuilds, Action::Remove);
static NODE_MODULES: Tag = tag("node_modules", Kind::Downloads, Action::Remove);

// find names the folder when space knows what it is and how to clean it. A build
// folder is known by what its tool leaves inside, as it may have any name; any tool
// may leave a CACHEDIR.TAG, cargo also leaves .rustc_info.json or names itself in it.
pub fn find(home: &Path, path: &Path) -> Option<&'static Tag> {
    if let Ok(relative) = path.strip_prefix(home) {
        if let Some((_, tag)) = BY_PATH.iter().find(|(known, _)| relative == Path::new(known)) {
            return Some(tag);
        }
    }
    if path.file_name()? == "node_modules" {
        return Some(&NODE_MODULES);
    }
    if path.join(".rustc_info.json").exists() {
        return Some(&RUST_TARGET);
    }
    let cachedir = fs::read_to_string(path.join("CACHEDIR.TAG")).ok()?;
    Some(if cachedir.contains("cargo") { &RUST_TARGET } else { &BUILD_CACHE })
}

// home_rebuilds lists the known folders in home that come back by themselves.
pub fn home_rebuilds(home: &Path) -> Vec<(PathBuf, &'static Tag)> {
    BY_PATH
        .iter()
        .filter(|(_, tag)| matches!(tag.kind, Kind::Rebuilds))
        .map(|(path, tag)| (home.join(path), tag))
        .filter(|(path, _)| path.exists())
        .collect()
}

impl Tag {
    pub fn describe(&self) -> String {
        let kind = match self.kind {
            Kind::Rebuilds => "rebuilds",
            Kind::Downloads => "re-downloads",
            Kind::Yours => "yours",
        };
        format!("{} · {kind}", self.label)
    }
}

// plan says what cleaning the path does, before doing it.
pub fn plan(tag: Option<&Tag>) -> String {
    match tag.map(|tag| &tag.action) {
        None => "move to Trash".into(),
        Some(Action::Remove) => "delete".into(),
        Some(Action::Empty) => "empty".into(),
        Some(Action::Run(command)) => command.join(" "),
        Some(Action::Manual(hint)) => format!("manual: {hint}"),
    }
}

pub fn clean(home: &Path, tag: Option<&Tag>, path: &Path) -> Result<(), String> {
    match tag.map(|tag| &tag.action) {
        None => move_to_trash(home, path),
        Some(Action::Remove) => remove(path),
        Some(Action::Empty) => {
            let entries = fs::read_dir(path).map_err(|err| format!("{}: {err}", path.display()))?;
            for entry in entries.flatten() {
                remove(&entry.path())?;
            }
            Ok(())
        }
        Some(Action::Run(command)) => {
            let output = Command::new(command[0])
                .args(&command[1..])
                .output()
                .map_err(|err| format!("{}: {err}", command.join(" ")))?;
            if !output.status.success() {
                return Err(format!("{}: {}", command.join(" "), String::from_utf8_lossy(&output.stderr).trim()));
            }
            Ok(())
        }
        Some(Action::Manual(hint)) => Err(format!("{}: do it in {hint}", path.display())),
    }
}

pub fn remove(path: &Path) -> Result<(), String> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) => Err(err),
    };
    result.map_err(|err| format!("{}: {err}", path.display()))
}

// move_to_trash keeps what space does not know, so a wrong pick can be undone;
// what is already in the Trash is deleted for good.
fn move_to_trash(home: &Path, path: &Path) -> Result<(), String> {
    let trash = home.join(".Trash");
    if path.starts_with(&trash) {
        return remove(path);
    }
    let name = path.file_name().ok_or_else(|| format!("{}: cannot move to Trash", path.display()))?;
    let mut destination = trash.join(name);
    let mut copy = 2;
    while destination.exists() {
        destination = trash.join(format!("{} {copy}", name.to_string_lossy()));
        copy += 1;
    }
    fs::rename(path, &destination).map_err(|err| format!("{}: {err}", path.display()))
}
