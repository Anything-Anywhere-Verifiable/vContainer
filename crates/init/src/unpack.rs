//! Layer unpack helper (`/init --stage unpack`). It opens the verified layer
//! at LAYER (outside the new root), then chroots into ROOT, so absolute
//! symlinks in the image resolve inside it. Entries apply in archive order,
//! as in containerd and moby: `.wh.<x>` removes x, `.wh..wh..opq` removes
//! what the directory held except this layer's own entries, and any other
//! entry replaces what was there unless both are directories.

use anyhow::{bail, ensure, Context, Result};
use flate2::read::MultiGzDecoder;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::{Component, Path, PathBuf};
use tar::{Archive, EntryType};

use crate::deploy::{LAYER, ROOT};
use crate::sys::{check, cstr};

pub fn main() -> Result<()> {
    let mut layer = File::open(LAYER).context(LAYER)?;
    check(unsafe { libc::chroot(cstr(ROOT).as_ptr()) }, "chroot")?;
    std::env::set_current_dir("/")?;
    let mut magic = [0u8; 4];
    let got = layer.read_at(&mut magic, 0)?;
    let reader: Box<dyn Read + '_> = if got >= 2 && magic[..2] == [0x1f, 0x8b] {
        Box::new(MultiGzDecoder::new(BufReader::new(&layer)))
    } else if got == 4 && magic == [0x28, 0xb5, 0x2f, 0xfd] {
        // Unbuffered, so the offset shows exactly where the frame ended.
        Box::new(ruzstd::decoding::StreamingDecoder::new(&layer).map_err(|e| anyhow::anyhow!("zstd: {e}"))?)
    } else {
        Box::new(BufReader::new(&layer))
    };
    let mut archive = Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_preserve_ownerships(true);
    archive.set_preserve_mtime(true);
    archive.set_unpack_xattrs(true);
    archive.set_overwrite(true);
    let mut extracted = HashSet::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = clean(&entry.path()?)?;
        let (Some(name), Some(dir)) = (path.file_name(), path.parent()) else { continue };
        let (dir, full) = (Path::new("/").join(dir), Path::new("/").join(&path));
        if name == ".wh..wh..opq" {
            opaque(&fs::canonicalize(&dir).unwrap_or(dir), &extracted)?;
        } else if let Some(target) = name.as_bytes().strip_prefix(b".wh.") {
            remove(&dir.join(OsStr::from_bytes(target)))?;
        } else {
            let kind = entry.header().entry_type();
            if !(kind.is_dir() && fs::symlink_metadata(&full).is_ok_and(|m| m.is_dir())) {
                remove(&full)?;
            }
            match kind {
                EntryType::Char | EntryType::Block => {} // device nodes come from the runtime's /dev
                EntryType::Fifo => {
                    let mode = entry.header().mode()? & 0o7777;
                    fs::create_dir_all(&dir)?;
                    let c = std::ffi::CString::new(full.as_os_str().as_bytes())?;
                    check(unsafe { libc::mkfifo(c.as_ptr(), mode) }, "mkfifo")?;
                    std::os::unix::fs::lchown(&full, Some(entry.header().uid()? as u32), Some(entry.header().gid()? as u32))?;
                    fs::set_permissions(&full, std::os::unix::fs::PermissionsExt::from_mode(mode))?;
                }
                _ => {
                    entry.unpack_in("/").with_context(|| format!("extract {}", path.display()))?;
                }
            }
            // Where the entry really is (its parent may be a symlink), and its
            // ancestors: an opaque marker hides only what lower layers left.
            let real = fs::canonicalize(&dir).unwrap_or(dir).join(name);
            for a in real.ancestors() {
                if !extracted.insert(a.to_path_buf()) {
                    break;
                }
            }
        }
    }
    // Whatever follows the tar end-of-archive must still be read, so the
    // rootfs never differs from what another runtime would extract.
    io::copy(&mut archive.into_inner(), &mut io::sink())?;
    let left = layer.metadata()?.len() - layer.stream_position()?;
    ensure!(left == 0, "layer has {left} unconsumed bytes after its last frame");
    Ok(())
}

/// Removes everything below `dir` (resolved) that this layer did not extract.
fn opaque(dir: &Path, extracted: &HashSet<PathBuf>) -> Result<()> {
    let Ok(children) = fs::read_dir(dir) else { return Ok(()) };
    for child in children {
        let p = child?.path();
        if !extracted.contains(&p) {
            remove(&p)?;
        } else if fs::symlink_metadata(&p)?.is_dir() {
            opaque(&p, extracted)?;
        }
    }
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
