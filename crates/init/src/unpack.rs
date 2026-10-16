//! Layer unpack helper (`/init --stage unpack ROOT`, layer on stdin).
//!
//! Runs chrooted into ROOT, so absolute symlinks in the image resolve inside
//! it. Each layer is read twice: first its whiteouts are applied to what the
//! lower layers left, then its other entries are extracted with OCI
//! replacement semantics (anything but dir-over-dir replaces what was there).

use anyhow::{bail, Context, Result};
use flate2::read::MultiGzDecoder;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::os::unix::io::FromRawFd;
use std::path::{Component, Path, PathBuf};
use tar::{Archive, Entry, EntryType};

use crate::sys::{check, cstr};

pub fn main(root: &str) -> Result<()> {
    check(unsafe { libc::chroot(cstr(root).as_ptr()) }, "chroot")?;
    std::env::set_current_dir("/")?;
    let mut layer = unsafe { File::from_raw_fd(0) };
    let len = layer.metadata()?.len();

    each_entry(&mut layer, len, |_, path| {
        let (Some(name), Some(dir)) = (path.file_name().and_then(|n| n.to_str()), path.parent()) else { return Ok(()) };
        if name == ".wh..wh..opq" {
            if let Ok(rd) = fs::read_dir(Path::new("/").join(dir)) {
                for child in rd {
                    remove(&child?.path())?;
                }
            }
        } else if let Some(target) = name.strip_prefix(".wh.") {
            remove(&Path::new("/").join(dir).join(target))?;
        }
        Ok(())
    })?;

    each_entry(&mut layer, len, |entry, path| {
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(".wh.")) {
            return Ok(());
        }
        let kind = entry.header().entry_type();
        let full = Path::new("/").join(path);
        if let Ok(old) = fs::symlink_metadata(&full) {
            if !(kind.is_dir() && old.is_dir()) {
                remove(&full)?;
            }
        }
        if matches!(kind, EntryType::Char | EntryType::Block) {
            return Ok(()); // device nodes come from the runtime's /dev
        }
        if kind == EntryType::Fifo {
            let mode = entry.header().mode()? & 0o7777;
            check(unsafe { libc::mkfifo(cstr(full.to_str().unwrap()).as_ptr(), mode) }, "mkfifo")?;
            let (uid, gid) = (entry.header().uid()? as u32, entry.header().gid()? as u32);
            std::os::unix::fs::lchown(&full, Some(uid), Some(gid))?;
            return Ok(());
        }
        entry.unpack_in("/").with_context(|| format!("extract {}", path.display()))?;
        Ok(())
    })?;
    println!("ok");
    Ok(())
}

fn remove(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p)?,
        Ok(_) => fs::remove_file(p)?,
        Err(_) => {}
    }
    Ok(())
}

fn each_entry(layer: &mut File, len: u64, mut f: impl FnMut(&mut Entry<Box<dyn Read>>, &Path) -> Result<()>) -> Result<()> {
    layer.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    let got = layer.read(&mut magic)?;
    layer.seek(SeekFrom::Start(0))?;
    // A duplicate descriptor shares the file offset with `layer`.
    let raw = layer.try_clone()?;
    let reader: Box<dyn Read> = if got >= 2 && magic[..2] == [0x1f, 0x8b] {
        Box::new(MultiGzDecoder::new(BufReader::new(raw)))
    } else if got == 4 && magic == [0x28, 0xb5, 0x2f, 0xfd] {
        // Unbuffered, so the offset shows exactly where the frame ended.
        Box::new(ruzstd::decoding::StreamingDecoder::new(raw).map_err(|e| anyhow::anyhow!("zstd: {e}"))?)
    } else {
        Box::new(BufReader::new(raw))
    };
    let mut archive = Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_preserve_ownerships(true);
    archive.set_preserve_mtime(true);
    archive.set_unpack_xattrs(true);
    archive.set_overwrite(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = clean(&entry.path()?)?;
        if path.as_os_str().is_empty() {
            continue;
        }
        f(&mut entry, &path)?;
    }
    // Whatever follows the tar end-of-archive must still be read, so the
    // rootfs never differs from what another runtime would extract.
    io::copy(&mut archive.into_inner(), &mut io::sink())?;
    let consumed = layer.stream_position()?;
    if consumed != len {
        bail!("layer has {} unconsumed bytes after its last frame", len - consumed);
    }
    Ok(())
}

/// A relative path without `.`/`..`/root components.
fn clean(p: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(x) => out.push(x),
            Component::CurDir | Component::RootDir => {}
            _ => bail!("unsafe path in layer: {}", p.display()),
        }
    }
    Ok(out)
}
