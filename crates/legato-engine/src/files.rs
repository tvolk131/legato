//! Sending and receiving files.
//!
//! Files travel one per stream, each with a [`FileHeader`] naming its batch (one send,
//! paste or drop) and its path within the batch, so folders keep their shape. Received
//! files go to `Downloads/Legato`, or to a scratch folder when they're for the clipboard.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use legato_net::{EndpointId, IncomingFile, Session};
use legato_proto::{FileHeader, FilePurpose};
use tokio::sync::mpsc;

/// The most sent in one go through the clipboard (explicit sends have no limit).
pub const MAX_CLIPBOARD_BATCH: u64 = 512 * 1024 * 1024;

/// A finished batch of received files.
#[derive(Debug, Clone, PartialEq)]
pub struct Received {
    pub from: EndpointId,
    pub batch: u64,
    pub purpose: FilePurpose,
    /// The top-level files and folders of the batch, where they were saved.
    pub paths: Vec<PathBuf>,
}

/// Where received files are saved.
pub fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|d| d.download_dir().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir)
        .join("Legato")
}

fn clipboard_dir(batch: u64) -> PathBuf {
    std::env::temp_dir()
        .join("legato-clipboard")
        .join(batch.to_string())
}

/// Turns a sender-supplied relative name into safe path components: no absolute paths,
/// no `..`, and no characters Windows can't store.
pub fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        let part = part.trim();
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        let cleaned: String = part
            .chars()
            .map(|c| {
                if "<>:\"|?*".contains(c) || c.is_control() {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
        if cleaned.is_empty() {
            return None;
        }
        out.push(cleaned);
    }
    if out.as_os_str().is_empty() {
        return None;
    }
    // Belt and braces: nothing that escapes.
    out.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(out)
}

/// `dir/name`, or `dir/name (2)` and so on if that's taken.
fn unique(dir: &Path, name: &Path) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = name
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| dir.join(name.with_file_name(format!("{stem} ({n}){ext}"))))
        .find(|p| !p.exists())
        .expect("some name is free")
}

/// Tracks incoming batches and saves their files.
pub(crate) struct Inbox {
    /// batch → (top-level name in the batch → where it was saved), files done.
    batches: HashMap<u64, (HashMap<PathBuf, PathBuf>, u32)>,
    done: mpsc::UnboundedSender<Result<(IncomingInfo, PathBuf)>>,
}

#[derive(Debug, Clone)]
pub(crate) struct IncomingInfo {
    pub from: EndpointId,
    pub header: FileHeader,
}

impl Inbox {
    pub(crate) fn new() -> (
        Self,
        mpsc::UnboundedReceiver<Result<(IncomingInfo, PathBuf)>>,
    ) {
        let (done, rx) = mpsc::unbounded_channel();
        (
            Self {
                batches: HashMap::new(),
                done,
            },
            rx,
        )
    }

    /// Starts saving an incoming file. The result arrives on the receiver from `new`.
    pub(crate) fn receive(&mut self, from: EndpointId, file: Arc<IncomingFile>) {
        let header = file.header.clone();
        let Some(relative) = safe_relative(&header.name) else {
            let _ = self.done.send(Err(anyhow::anyhow!(
                "refused a file named {:?}",
                header.name
            )));
            return;
        };
        let root = match header.purpose {
            FilePurpose::Clipboard => clipboard_dir(header.batch),
            FilePurpose::Send | FilePurpose::Drop => downloads_dir(),
        };
        // Everything in a batch shares one top-level folder or file name, chosen once so a
        // second send of "Photos" becomes "Photos (2)" rather than merging.
        let (tops, _) = self.batches.entry(header.batch).or_default();
        let first: PathBuf = relative
            .components()
            .next()
            .map(|c| c.as_os_str().into())
            .unwrap_or_default();
        let top = tops
            .entry(first.clone())
            .or_insert_with(|| unique(&root, &first))
            .clone();
        let rest: PathBuf = relative.components().skip(1).collect();
        let path = if rest.as_os_str().is_empty() {
            top
        } else {
            top.join(rest)
        };
        let done = self.done.clone();
        tokio::spawn(async move {
            let result = save(&file, &path).await.map(|()| {
                (
                    IncomingInfo {
                        from,
                        header: file.header.clone(),
                    },
                    path,
                )
            });
            let _ = done.send(result);
        });
    }

    /// Records a saved file; returns the batch once all of it has arrived.
    pub(crate) fn finished(&mut self, info: &IncomingInfo) -> Option<Received> {
        let (_, done) = self.batches.get_mut(&info.header.batch)?;
        *done += 1;
        if *done < info.header.count {
            return None;
        }
        let (tops, _) = self.batches.remove(&info.header.batch)?;
        let mut paths: Vec<PathBuf> = tops.into_values().collect();
        paths.sort();
        Some(Received {
            from: info.from,
            batch: info.header.batch,
            purpose: info.header.purpose,
            paths,
        })
    }
}

async fn save(file: &IncomingFile, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut out = tokio::fs::File::create(path)
        .await
        .with_context(|| format!("creating {}", path.display()))?;
    if let Err(e) = file.recv(&mut out).await {
        drop(out);
        let _ = tokio::fs::remove_file(path).await;
        return Err(e);
    }
    Ok(())
}

/// Every file under `paths` (folders are walked), with its name relative to the batch.
pub fn collect(paths: &[PathBuf]) -> Result<Vec<(PathBuf, String, u64)>> {
    let mut out = Vec::new();
    for root in paths {
        let base = root.parent().unwrap_or(Path::new(""));
        let mut stack = vec![root.clone()];
        while let Some(path) = stack.pop() {
            let meta =
                std::fs::metadata(&path).with_context(|| format!("reading {}", path.display()))?;
            if meta.is_dir() {
                for entry in std::fs::read_dir(&path)? {
                    stack.push(entry?.path());
                }
            } else if meta.is_file() {
                let rel = path.strip_prefix(base).unwrap_or(&path);
                let name = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((path, name, meta.len()));
            }
        }
    }
    if out.is_empty() {
        bail!("nothing to send");
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// Sends files (and folders) to a peer as one batch.
pub async fn send(session: &Session, paths: &[PathBuf], purpose: FilePurpose) -> Result<u64> {
    let files = collect(paths)?;
    let batch = rand_batch();
    let count = files.len() as u32;
    let mut total = 0;
    for (index, (path, name, size)) in files.into_iter().enumerate() {
        let mut file = tokio::fs::File::open(&path)
            .await
            .with_context(|| format!("opening {}", path.display()))?;
        let header = FileHeader {
            batch,
            name,
            size,
            count,
            index: index as u32,
            purpose,
        };
        session.send_file(header, &mut file).await?;
        total += size;
    }
    Ok(total)
}

fn rand_batch() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (std::process::id() as u64) << 32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_names_are_refused_or_cleaned() {
        assert_eq!(safe_relative("../../etc/passwd"), None);
        assert_eq!(safe_relative("/"), None);
        assert_eq!(
            safe_relative("a/./b.txt"),
            Some(PathBuf::from("a").join("b.txt"))
        );
        assert_eq!(
            safe_relative("/abs/x"),
            Some(PathBuf::from("abs").join("x"))
        );
        assert_eq!(safe_relative("what?.txt"), Some(PathBuf::from("what_.txt")));
        assert_eq!(safe_relative("trailing. "), Some(PathBuf::from("trailing")));
        assert_eq!(
            safe_relative(r"win\style\name.doc"),
            Some(PathBuf::from("win").join("style").join("name.doc"))
        );
    }

    #[test]
    fn taken_names_get_numbered() {
        let dir = std::env::temp_dir().join(format!("legato-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::write(dir.join("a (2).txt"), "").unwrap();
        assert_eq!(unique(&dir, Path::new("a.txt")), dir.join("a (3).txt"));
        assert_eq!(unique(&dir, Path::new("b.txt")), dir.join("b.txt"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folders_are_walked_with_relative_names() {
        let dir = std::env::temp_dir().join(format!("legato-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Photos/2026")).unwrap();
        std::fs::write(dir.join("Photos/a.jpg"), "aa").unwrap();
        std::fs::write(dir.join("Photos/2026/b.jpg"), "bbb").unwrap();
        std::fs::write(dir.join("notes.txt"), "n").unwrap();
        let files = collect(&[dir.join("Photos"), dir.join("notes.txt")]).unwrap();
        let names: Vec<_> = files.iter().map(|(_, n, s)| (n.as_str(), *s)).collect();
        assert_eq!(
            names,
            [
                ("Photos/2026/b.jpg", 3),
                ("Photos/a.jpg", 2),
                ("notes.txt", 1)
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
