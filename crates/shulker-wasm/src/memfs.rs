//! In-memory filesystem and single-threaded loader driver for `bedrock_addon`'s sans-IO loaders.

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;

use bedrock::addon::load::{
    DirEntry, EntryKind, Input, ListDepth, Loader, Output, Place, ReadRange, Request, RootId,
};
use bedrock::addon::storage::PackPath;
use bytes::Bytes;

/// One virtual root registered in [`MemFs`]: either a raw archive or an in-memory directory tree.
enum MemRoot {
    Archive {
        name: String,
        bytes: Bytes,
    },
    Dir {
        name: String,
        files: BTreeMap<PackPath, Bytes>,
    },
}

impl MemRoot {
    fn name(&self) -> &str {
        match self {
            MemRoot::Archive { name, .. } | MemRoot::Dir { name, .. } => name,
        }
    }
}

/// Virtual root table + synchronous sans-IO loader driver.
#[derive(Default)]
pub struct MemFs {
    roots: Vec<MemRoot>,
}

impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_archive(&mut self, name: String, bytes: Vec<u8>) -> RootId {
        let index = u32::try_from(self.roots.len())
            .ok()
            .filter(|&index| index != RootId::MEMORY.0)
            .expect("root table full");
        self.roots.push(MemRoot::Archive {
            name,
            bytes: Bytes::from(bytes),
        });
        RootId(index)
    }

    pub fn add_dir(&mut self, name: String, files: BTreeMap<PackPath, Bytes>) -> RootId {
        let index = u32::try_from(self.roots.len())
            .ok()
            .filter(|&index| index != RootId::MEMORY.0)
            .expect("root table full");
        self.roots.push(MemRoot::Dir { name, files });
        RootId(index)
    }

    /// Display path of `place` (`<root_name>` or `<root_name>/<path>`), or `None` for
    /// `RootId::MEMORY` / unknown roots.
    pub fn display_place(&self, place: &Place) -> Option<String> {
        let root = self.get_root(place.root)?;
        if place.path.is_root() {
            Some(root.name().to_owned())
        } else {
            Some(format!("{}/{}", root.name(), place.path))
        }
    }

    /// Reads a file directly from a [`MemRoot::Dir`], if `place` points into one.
    pub fn read_dir_file(&self, place: &Place) -> Option<Bytes> {
        let MemRoot::Dir { files, .. } = self.get_root(place.root)? else {
            return None;
        };
        files.get(&place.path).cloned()
    }

    fn get_root(&self, root: RootId) -> Option<&MemRoot> {
        if root == RootId::MEMORY {
            return None;
        }
        let idx = usize::try_from(root.0).ok()?;
        self.roots.get(idx)
    }

    /// Answers a single [`Request`] from in-memory files.
    pub fn respond(&self, request: Request) -> Input {
        match request {
            Request::ReadFile {
                location, range, ..
            } => self.respond_read(location, range),
            Request::ListDir { location, depth } => self.respond_list(location, depth),
        }
    }

    fn respond_read(&self, location: Place, range: ReadRange) -> Input {
        let Some(root) = self.get_root(location.root) else {
            return Input::Failed {
                location,
                kind: ErrorKind::InvalidInput,
            };
        };
        match root {
            MemRoot::Archive { bytes, .. } => {
                if location.path.is_root() {
                    Input::File {
                        location,
                        bytes: slice_range(bytes, range),
                    }
                } else {
                    Input::Missing { location }
                }
            }
            MemRoot::Dir { files, .. } => {
                if let Some(bytes) = files.get(&location.path) {
                    return Input::File {
                        location,
                        bytes: slice_range(bytes, range),
                    };
                }
                if location.path.is_root() || has_dir_prefix(files, &location.path) {
                    return Input::Failed {
                        location,
                        kind: ErrorKind::IsADirectory,
                    };
                }
                Input::Missing { location }
            }
        }
    }

    fn respond_list(&self, location: Place, depth: ListDepth) -> Input {
        let Some(root) = self.get_root(location.root) else {
            return Input::Failed {
                location,
                kind: ErrorKind::InvalidInput,
            };
        };
        let MemRoot::Dir { files, .. } = root else {
            return Input::Failed {
                location,
                kind: ErrorKind::NotADirectory,
            };
        };

        if !location.path.is_root() {
            if files.contains_key(&location.path) {
                return Input::Failed {
                    location,
                    kind: ErrorKind::NotADirectory,
                };
            }
            if !has_dir_prefix(files, &location.path) {
                return Input::Missing { location };
            }
        }

        let entries = list_entries(files, &location.path, depth);
        Input::Dir { location, entries }
    }

    /// Drives any sans-IO `Loader` to completion on the calling thread.
    pub fn drive<T, L: Loader<T>>(&self, mut loader: L) -> Result<T, String> {
        loop {
            let Some(output) = loader.poll() else {
                return Err("loader stalled with no pending output".to_owned());
            };
            let input = match output {
                Output::Request(req) => self.respond(req),
                Output::Job(job) => Input::JobDone(job.run()),
                Output::NeedKey(req) => Input::Key {
                    identity: req.identity,
                    key: None,
                },
                Output::Done(value) => return Ok(value),
            };
            loader
                .handle(input)
                .map_err(|err| format!("loader protocol error: {err}"))?;
        }
    }
}

fn slice_range(bytes: &Bytes, range: ReadRange) -> Bytes {
    match range {
        ReadRange::Whole => bytes.clone(),
        ReadRange::Prefix(n) => {
            let end = (n as usize).min(bytes.len());
            bytes.slice(..end)
        }
    }
}

fn has_dir_prefix(files: &BTreeMap<PackPath, Bytes>, dir: &PackPath) -> bool {
    let prefix = dir.as_str();
    files.keys().any(|path| {
        let s = path.as_str();
        s.len() > prefix.len()
            && s.starts_with(prefix)
            && s.as_bytes()[prefix.len()] == b'/'
    })
}

fn strip_dir_prefix<'a>(path: &'a str, dir: &PackPath) -> Option<&'a str> {
    if dir.is_root() {
        return Some(path);
    }
    let prefix = dir.as_str();
    if path.len() > prefix.len()
        && path.starts_with(prefix)
        && path.as_bytes()[prefix.len()] == b'/'
    {
        Some(&path[prefix.len() + 1..])
    } else {
        None
    }
}

fn list_entries(files: &BTreeMap<PackPath, Bytes>, dir: &PackPath, depth: ListDepth) -> Vec<DirEntry> {
    let mut seen_dirs: BTreeSet<String> = BTreeSet::new();
    let mut entries: Vec<DirEntry> = Vec::new();

    for (path, bytes) in files {
        let Some(rel) = strip_dir_prefix(path.as_str(), dir) else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        match depth {
            ListDepth::Children => {
                if let Some((first, _)) = rel.split_once('/') {
                    if seen_dirs.insert(first.to_owned()) {
                        entries.push(DirEntry {
                            name: first.into(),
                            lossy: false,
                            kind: EntryKind::Dir,
                        });
                    }
                } else {
                    entries.push(DirEntry {
                        name: rel.into(),
                        lossy: false,
                        kind: EntryKind::File {
                            len: bytes.len() as u64,
                        },
                    });
                }
            }
            ListDepth::Recursive => {
                let mut offset = 0;
                while let Some(pos) = rel[offset..].find('/') {
                    let sub_end = offset + pos;
                    let sub_dir = &rel[..sub_end];
                    if seen_dirs.insert(sub_dir.to_owned()) {
                        entries.push(DirEntry {
                            name: sub_dir.into(),
                            lossy: false,
                            kind: EntryKind::Dir,
                        });
                    }
                    offset = sub_end + 1;
                }
                entries.push(DirEntry {
                    name: rel.into(),
                    lossy: false,
                    kind: EntryKind::File {
                        len: bytes.len() as u64,
                    },
                });
            }
        }
    }

    entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    entries
}
