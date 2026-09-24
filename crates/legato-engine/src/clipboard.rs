//! Keeping the clipboard in sync with paired machines.
//!
//! A thread checks the OS's cheap clipboard change counter a few times a second; when
//! something new is copied here it reads it and hands it on. Contents that arrive from a
//! peer are written to the clipboard and remembered, so they aren't echoed back.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use legato_proto::ClipboardContent;

use crate::platform;

const POLL: Duration = Duration::from_millis(250);
/// Text or images bigger than this aren't shared (compressed images are usually far
/// smaller than raw pixels, so this is about raw size).
const MAX_TEXT: usize = 8 * 1024 * 1024;
const MAX_PIXELS: usize = 8192 * 8192;

/// Something the user copied on this machine.
#[derive(Debug, Clone, PartialEq, Hash)]
pub(crate) enum Copied {
    Content(ClipboardContent),
    Files(Vec<PathBuf>),
}

enum Command {
    Set(Copied),
    Stop,
}

pub(crate) struct ClipboardSync {
    commands: mpsc::Sender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ClipboardSync {
    /// Starts watching. `on_copy` is called (on the watcher thread) with anything the user
    /// copies on this machine.
    pub(crate) fn start(on_copy: impl FnMut(Copied) + Send + 'static) -> Option<Self> {
        let (commands, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("legato-clipboard".into())
            .spawn(move || watch(rx, on_copy))
            .ok()?;
        Some(Self {
            commands,
            thread: Some(thread),
        })
    }

    /// Puts contents from a peer on this machine's clipboard.
    pub(crate) fn set(&self, content: ClipboardContent) {
        let _ = self.commands.send(Command::Set(Copied::Content(content)));
    }

    /// Puts received files on this machine's clipboard, ready to paste.
    pub(crate) fn set_files(&self, paths: Vec<PathBuf>) {
        let _ = self.commands.send(Command::Set(Copied::Files(paths)));
    }
}

impl Drop for ClipboardSync {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn hash(content: &Copied) -> u64 {
    let mut h = DefaultHasher::new();
    content.hash(&mut h);
    h.finish()
}

fn watch(rx: mpsc::Receiver<Command>, mut on_copy: impl FnMut(Copied)) {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("clipboard sharing is off: {e}");
            return;
        }
    };
    let mut seen = platform::clipboard_change_count();
    // What's on the clipboard because a peer put it there.
    let mut from_peer: Option<u64> = None;
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Command::Set(content)) => {
                from_peer = Some(hash(&content));
                if let Err(e) = write(&mut clipboard, &content) {
                    tracing::debug!("couldn't set the clipboard: {e:#}");
                }
                seen = platform::clipboard_change_count();
            }
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                let now = platform::clipboard_change_count();
                if now == seen {
                    continue;
                }
                seen = now;
                let Some(content) = read(&mut clipboard) else {
                    continue;
                };
                if from_peer == Some(hash(&content)) {
                    continue;
                }
                from_peer = None;
                on_copy(content);
            }
        }
    }
}

fn read(clipboard: &mut arboard::Clipboard) -> Option<Copied> {
    // Files first: copying files in Finder also puts their names on the clipboard as text.
    if let Ok(files) = clipboard.get().file_list()
        && !files.is_empty()
    {
        return Some(Copied::Files(files));
    }
    if let Ok(text) = clipboard.get_text() {
        return (!text.is_empty() && text.len() <= MAX_TEXT)
            .then_some(Copied::Content(ClipboardContent::Text(text)));
    }
    let image = clipboard.get_image().ok()?;
    if image.width * image.height > MAX_PIXELS {
        return None;
    }
    encode_png(image.width as u32, image.height as u32, &image.bytes)
        .map(|png| Copied::Content(ClipboardContent::Image { png }))
}

fn write(clipboard: &mut arboard::Clipboard, content: &Copied) -> anyhow::Result<()> {
    match content {
        Copied::Files(paths) => clipboard.set().file_list(paths)?,
        Copied::Content(ClipboardContent::Text(text)) => clipboard.set_text(text.clone())?,
        Copied::Content(ClipboardContent::Image { png }) => {
            let (width, height, rgba) = decode_png(png)?;
            clipboard.set_image(arboard::ImageData {
                width: width as usize,
                height: height as usize,
                bytes: rgba.into(),
            })?;
        }
    }
    Ok(())
}

pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

pub(crate) fn decode_png(png: &[u8]) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    anyhow::ensure!(
        info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight,
        "unsupported image format"
    );
    Ok((info.width, info.height, buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_survive_the_trip() {
        let (w, h) = (3u32, 2u32);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 11) as u8).collect();
        let png = encode_png(w, h, &rgba).unwrap();
        assert_eq!(decode_png(&png).unwrap(), (w, h, rgba));
    }
}
