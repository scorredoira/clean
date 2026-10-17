use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use crate::known::{self, Tag};

// MIN is the smallest entry kept as its own row; smaller ones only add to their folder.
pub const MIN: u64 = 1_000_000;

pub struct Node {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
    pub tag: Option<&'static Tag>,
    pub children: Vec<Node>,
}

pub struct Scanner {
    pub home: PathBuf,
    pub entries: AtomicU64,
    pub bytes: AtomicU64,
    device: u64,
}

impl Scanner {
    // new scans on the volume of base; home places the known folders.
    pub fn new(home: PathBuf, base: &Path) -> Scanner {
        let device = fs::metadata(base).map(|meta| meta.dev()).unwrap_or(0);
        Scanner { home, entries: AtomicU64::new(0), bytes: AtomicU64::new(0), device }
    }

    // scan sizes the path by its allocated blocks, staying on one volume.
    pub fn scan(&self, path: &Path) -> Option<Node> {
        self.scan_reusing(path, None)
    }

    // scan_parent scans the folder holding the node, keeping the node as it is.
    pub fn scan_parent(&self, node: Node) -> Option<Node> {
        let parent = node.path.parent()?.to_path_buf();
        self.scan_reusing(&parent, Some(node))
    }

    fn scan_reusing(&self, path: &Path, reused: Option<Node>) -> Option<Node> {
        let meta = fs::symlink_metadata(path).ok()?;
        let own = meta.blocks() * 512;
        self.entries.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(own, Ordering::Relaxed);
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        let mut node = Node { name, path: path.to_path_buf(), size: own, is_dir: meta.is_dir(), tag: None, children: Vec::new() };
        if !meta.is_dir() || meta.dev() != self.device {
            return Some(node);
        }
        node.tag = known::find(&self.home, path);
        let Ok(entries) = fs::read_dir(path) else {
            return Some(node);
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        if let Some(reused) = &reused {
            paths.retain(|path| *path != reused.path);
        }
        let mut children: Vec<Node> = paths.par_iter().filter_map(|path| self.scan(path)).collect();
        children.extend(reused);
        node.size += children.iter().map(|child| child.size).sum::<u64>();
        children.retain(|child| child.size >= MIN);
        children.sort_by(|a, b| b.size.cmp(&a.size));
        node.children = children;
        Some(node)
    }
}

impl Node {
    pub fn find(&self, path: &Path) -> Option<&Node> {
        let relative = path.strip_prefix(&self.path).ok()?;
        let mut node = self;
        for part in relative.iter() {
            node = node.children.iter().find(|child| child.name == part.to_string_lossy())?;
        }
        Some(node)
    }

    // refresh scans the path again and carries the change in size up to this node.
    pub fn refresh(&mut self, scanner: &Scanner, path: &Path) {
        let Ok(relative) = path.strip_prefix(&self.path) else {
            return;
        };
        let parts: Vec<String> = relative.iter().map(|part| part.to_string_lossy().into_owned()).collect();
        if !parts.is_empty() {
            self.refresh_parts(scanner, &parts);
        }
    }

    fn refresh_parts(&mut self, scanner: &Scanner, parts: &[String]) -> i64 {
        let index = self.children.iter().position(|child| child.name == parts[0]);
        let delta = if parts.len() == 1 {
            let old = index.map_or(0, |index| self.children[index].size);
            let new = scanner.scan(&self.path.join(&parts[0])).filter(|node| node.size >= MIN);
            let size = new.as_ref().map_or(0, |node| node.size);
            match (index, new) {
                (Some(index), Some(node)) => self.children[index] = node,
                (Some(index), None) => {
                    self.children.remove(index);
                }
                (None, Some(node)) => self.children.push(node),
                (None, None) => {}
            }
            size as i64 - old as i64
        } else {
            match index {
                Some(index) => self.children[index].refresh_parts(scanner, &parts[1..]),
                None => 0,
            }
        };
        self.size = self.size.saturating_add_signed(delta);
        self.children.sort_by(|a, b| b.size.cmp(&a.size));
        delta
    }

    // tagged collects the known folders, without the ones nested inside another.
    pub fn tagged<'a>(&'a self, out: &mut Vec<&'a Node>) {
        for child in &self.children {
            if child.tag.is_some() {
                out.push(child);
            } else {
                child.tagged(out);
            }
        }
    }
}
