//! Sending and receiving files.
//!
//! Files travel one per stream, each with a [`FileHeader`] naming its batch (one send,
//! paste or drop) and its path within the batch, so folders keep their shape. Received
//! files go to `Downloads/Legato`, or to a scratch folder when they're for the clipboard.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::{fs, io};

use anyhow::{Context, Result, bail};
use legato_net::{EndpointId, IncomingFile, Session};
use legato_proto::{FileHeader, FilePurpose};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

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

/// Turns a sender-supplied relative name into safe path components: no absolute paths,
/// no `..`, and no characters Windows can't store.
pub fn safe_relative(name: &str) -> Option<PathBuf> {
    Some(
        safe_parts(name)?
            .into_iter()
            .map(|(_, clean)| clean)
            .collect(),
    )
}

/// Keep the original component as its identity: different names can sanitize alike.
fn safe_parts(name: &str) -> Option<Vec<(&str, String)>> {
    let mut parts = Vec::new();
    for original in name.split(['/', '\\']) {
        let part = original.trim();
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
        if cleaned.is_empty()
            || !Path::new(&cleaned)
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
        {
            return None;
        }
        parts.push((original, cleaned));
    }
    (!parts.is_empty()).then_some(parts)
}

/// Reserve a name atomically. The filesystem decides what collides (including case).
fn reserve<T>(
    dir: &Path,
    name: &Path,
    mut create: impl FnMut(&Path) -> io::Result<T>,
) -> Result<(PathBuf, T)> {
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let ext = name
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 1u64.. {
        let path = dir.join(if n == 1 {
            name.to_path_buf()
        } else {
            PathBuf::from(format!("{stem} ({n}){ext}"))
        });
        match create(&path) {
            Ok(value) => return Ok((path, value)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
        }
    }
    unreachable!("some name is free")
}

struct Batch {
    /// Original directory prefixes → exclusively created destination directories.
    directories: HashMap<Vec<String>, PathBuf>,
    roots: Vec<PathBuf>,
    done: u32,
    count: u32,
    purpose: Option<FilePurpose>,
    indices: HashSet<u32>,
    generation: u64,
    cancel: watch::Sender<()>,
}

impl Default for Batch {
    fn default() -> Self {
        Self {
            directories: HashMap::new(),
            roots: Vec::new(),
            done: 0,
            count: 0,
            purpose: None,
            indices: HashSet::new(),
            generation: 0,
            cancel: watch::channel(()).0,
        }
    }
}

struct Destination {
    path: PathBuf,
    file: fs::File,
}

impl Batch {
    fn reserve(&mut self, root: &Path, name: &str) -> Result<Destination> {
        let parts = safe_parts(name).with_context(|| format!("refused a file named {name:?}"))?;
        fs::create_dir_all(root)?;
        let mut parent = root.to_path_buf();
        let mut original = Vec::new();
        for (depth, (part, clean)) in parts.iter().enumerate() {
            original.push((*part).to_string());
            if depth == parts.len() - 1 {
                let (path, file) = reserve(&parent, Path::new(clean), |p| {
                    fs::OpenOptions::new().write(true).create_new(true).open(p)
                })?;
                if depth == 0 {
                    self.roots.push(path.clone());
                }
                return Ok(Destination { path, file });
            }
            parent = match self.directories.get(&original) {
                Some(path) => path.clone(),
                None => {
                    let (path, ()) = reserve(&parent, Path::new(clean), |p| fs::create_dir(p))?;
                    if depth == 0 {
                        self.roots.push(path.clone());
                    }
                    self.directories.insert(original.clone(), path.clone());
                    path
                }
            };
        }
        unreachable!("a safe path has at least one component")
    }
}

/// Tracks incoming batches and saves their files.
pub(crate) struct Inbox {
    batches: HashMap<(EndpointId, u64), Batch>,
    downloads: PathBuf,
    clipboard: PathBuf,
    done: mpsc::UnboundedSender<Completion>,
    generation: u64,
    cancelled: VecDeque<(EndpointId, u64)>,
}

#[derive(Debug, Clone)]
pub(crate) struct IncomingInfo {
    pub from: EndpointId,
    pub header: FileHeader,
    generation: u64,
}

pub(crate) type Completion = (IncomingInfo, Result<PathBuf>);

impl Inbox {
    pub(crate) fn new() -> (Self, mpsc::UnboundedReceiver<Completion>) {
        Self::with_roots(
            downloads_dir(),
            std::env::temp_dir().join("legato-clipboard"),
        )
    }

    fn with_roots(
        downloads: PathBuf,
        clipboard: PathBuf,
    ) -> (Self, mpsc::UnboundedReceiver<Completion>) {
        let (done, rx) = mpsc::unbounded_channel();
        (
            Self {
                batches: HashMap::new(),
                downloads,
                clipboard,
                done,
                generation: 0,
                cancelled: VecDeque::new(),
            },
            rx,
        )
    }

    /// Starts saving an incoming file. The result arrives on the receiver from `new`.
    pub(crate) fn receive(&mut self, from: EndpointId, file: Arc<IncomingFile>) {
        let destination = self.reserve(from, &file.header);
        let batch = self.batches.get(&(from, file.header.batch));
        let info = IncomingInfo {
            from,
            header: file.header.clone(),
            generation: batch.map_or(0, |b| b.generation),
        };
        let mut cancel = batch.map(|b| b.cancel.subscribe());
        let done = self.done.clone();
        tokio::spawn(async move {
            let result = match destination {
                Ok(destination) => {
                    let path = destination.path.clone();
                    save(&file, destination, cancel.as_mut().unwrap())
                        .await
                        .map(|()| path)
                }
                Err(e) => Err(e),
            };
            if let Err(e) = file
                .complete(
                    &result
                        .as_ref()
                        .map(|_| ())
                        .map_err(|e| anyhow::anyhow!("{e:#}")),
                )
                .await
            {
                tracing::debug!("couldn't acknowledge file save: {e:#}");
            }
            let _ = done.send((info, result));
        });
    }

    fn reserve(&mut self, from: EndpointId, header: &FileHeader) -> Result<Destination> {
        let root = match header.purpose {
            FilePurpose::Clipboard => self
                .clipboard
                .join(from.to_string())
                .join(header.batch.to_string()),
            FilePurpose::Send | FilePurpose::Drop => self.downloads.clone(),
        };
        let key = (from, header.batch);
        if self.cancelled.contains(&key) {
            bail!("file batch was cancelled");
        }
        if header.count == 0 || header.index >= header.count {
            bail!("invalid file index/count");
        }
        if let Some(batch) = self.batches.get_mut(&key) {
            if batch.count != header.count
                || batch.purpose != Some(header.purpose)
                || batch.indices.contains(&header.index)
            {
                bail!("inconsistent or duplicate file in batch");
            }
            let destination = batch.reserve(&root, &header.name)?;
            batch.indices.insert(header.index);
            Ok(destination)
        } else {
            self.generation += 1;
            let mut batch = Batch {
                count: header.count,
                purpose: Some(header.purpose),
                generation: self.generation,
                ..Default::default()
            };
            let destination = batch.reserve(&root, &header.name)?;
            batch.indices.insert(header.index);
            self.batches.insert(key, batch);
            Ok(destination)
        }
    }

    /// Cancel unfinished saves without deleting files already saved. A short bounded
    /// history rejects file headers that race a cancellation on the control stream.
    pub(crate) fn cancel(&mut self, from: EndpointId, batch: u64) {
        let key = (from, batch);
        self.batches.remove(&key); // Closing the watch channel cancels its save tasks.
        if !self.cancelled.contains(&key) {
            self.cancelled.push_back(key);
            if self.cancelled.len() > 256 {
                self.cancelled.pop_front();
            }
        }
    }

    pub(crate) fn disconnected(&mut self, from: EndpointId) {
        self.batches.retain(|(peer, _), _| *peer != from);
        self.cancelled.retain(|(peer, _)| *peer != from);
    }

    pub(crate) fn failed(&mut self, info: &IncomingInfo) {
        if self
            .batches
            .get(&(info.from, info.header.batch))
            .is_none_or(|b| b.generation == info.generation)
        {
            self.cancel(info.from, info.header.batch);
        }
    }

    /// Records a saved file; returns the batch once all of it has arrived.
    pub(crate) fn finished(&mut self, info: &IncomingInfo) -> Option<Received> {
        let key = (info.from, info.header.batch);
        let batch = self.batches.get_mut(&key)?;
        if batch.generation != info.generation {
            return None;
        }
        batch.done += 1;
        if batch.done < info.header.count {
            return None;
        }
        let mut paths = self.batches.remove(&key)?.roots;
        paths.sort();
        Some(Received {
            from: info.from,
            batch: info.header.batch,
            purpose: info.header.purpose,
            paths,
        })
    }
}

async fn save(
    file: &IncomingFile,
    destination: Destination,
    cancel: &mut watch::Receiver<()>,
) -> Result<()> {
    // This handle was opened with create_new before the receive task was started.
    // Never reopen/truncate the path another transfer may be using.
    let mut out = tokio::fs::File::from_std(destination.file);
    let result = receive_into(file, &mut out, cancel).await;
    drop(out);
    if result.is_err() {
        let _ = tokio::fs::remove_file(destination.path).await;
    }
    result
}

async fn receive_into(
    file: &IncomingFile,
    out: &mut (impl tokio::io::AsyncWrite + Unpin),
    cancel: &mut watch::Receiver<()>,
) -> Result<()> {
    let received = tokio::select! {
        biased;
        _ = cancel.changed() => Err(anyhow::anyhow!("file batch was cancelled")),
        result = file.recv(out) => result,
    };
    // Drain outstanding filesystem writes even after a stream error/cancellation,
    // before closing the handle and deleting only this transfer's partial file.
    let flushed = out.flush().await;
    received.and_then(|_| flushed.map_err(Into::into))
}

/// Every file under `paths`, with its name relative to the selection. Links inside a
/// selected folder may resolve within that folder, but may not escape it or form cycles.
/// An explicitly selected link selects its target. Return resolved paths so changing a
/// link after the scan cannot redirect a send to a different target.
pub fn collect(paths: &[PathBuf]) -> Result<Vec<(PathBuf, String, u64)>> {
    enum Step {
        Visit(PathBuf),
        Leave(PathBuf),
    }
    let mut out = Vec::new();
    for root in paths {
        let base = root.parent().unwrap_or(Path::new(""));
        let boundary =
            fs::canonicalize(root).with_context(|| format!("reading {}", root.display()))?;
        let mut ancestors = HashSet::new();
        let mut stack = vec![Step::Visit(root.clone())];
        while let Some(step) = stack.pop() {
            let path = match step {
                Step::Visit(path) => path,
                Step::Leave(path) => {
                    ancestors.remove(&path);
                    continue;
                }
            };
            let resolved =
                fs::canonicalize(&path).with_context(|| format!("resolving {}", path.display()))?;
            if !resolved.starts_with(&boundary) {
                bail!(
                    "{} links outside the selected folder; select its target separately to send it",
                    path.display()
                );
            }
            let meta =
                fs::metadata(&resolved).with_context(|| format!("reading {}", path.display()))?;
            if meta.is_dir() {
                if !ancestors.insert(resolved.clone()) {
                    bail!("{} forms a folder link cycle", path.display());
                }
                stack.push(Step::Leave(resolved));
                for entry in fs::read_dir(&path)? {
                    stack.push(Step::Visit(entry?.path()));
                }
            } else if meta.is_file() {
                let rel = path.strip_prefix(base).unwrap_or(&path);
                let name = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((resolved, name, meta.len()));
            }
        }
    }
    if out.is_empty() {
        bail!("nothing to send");
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// Walk before opening any streams, on the blocking pool rather than the sharing loop
/// or a Tokio worker. The same manifest supplies clipboard limits and transfer headers.
async fn prepare(
    purpose: FilePurpose,
    scan: impl FnOnce() -> Result<Vec<(PathBuf, String, u64)>> + Send + 'static,
) -> Result<Vec<(PathBuf, String, u64)>> {
    let files = tokio::task::spawn_blocking(scan)
        .await
        .context("scanning files")??;
    let total = files
        .iter()
        .try_fold(0u64, |total, (_, _, size)| total.checked_add(*size))
        .context("file batch size overflow")?;
    if purpose == FilePurpose::Clipboard && total > MAX_CLIPBOARD_BATCH {
        bail!("Copied files are too big to share through the clipboard; send them instead.");
    }
    Ok(files)
}

/// Sends files (and folders) to a peer as one batch.
pub async fn send(session: &Session, paths: &[PathBuf], purpose: FilePurpose) -> Result<u64> {
    let paths = paths.to_vec();
    let files = prepare(purpose, move || collect(&paths)).await?;
    let batch = rand_batch();
    let count = files.len().try_into().context("too many files")?;
    let mut pending = PendingBatch {
        session,
        batch,
        complete: false,
    };
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
    pending.complete = true;
    Ok(total)
}

struct PendingBatch<'a> {
    session: &'a Session,
    batch: u64,
    complete: bool,
}

impl Drop for PendingBatch<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.session
                .send(legato_proto::Control::FileCancel { batch: self.batch });
        }
    }
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
        assert_eq!(
            reserve(&dir, Path::new("a.txt"), |p| fs::File::create_new(p))
                .unwrap()
                .0,
            dir.join("a (3).txt")
        );
        assert_eq!(
            reserve(&dir, Path::new("b.txt"), |p| fs::File::create_new(p))
                .unwrap()
                .0,
            dir.join("b.txt")
        );
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

#[cfg(test)]
#[path = "files/tests.rs"]
mod receive_tests;

#[cfg(test)]
#[path = "files/collect_tests.rs"]
mod collect_tests;
