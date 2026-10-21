//! Layer unpack helper (`/init --stage unpack`). It opens the verified layer
//! at LAYER (outside the new root), then chroots into ROOT, so absolute
//! symlinks in the image resolve inside it. Entries apply in archive order,
//! but whiteouts remove only lower-layer contents: `.wh.<x>` removes x and
//! `.wh..wh..opq` clears its directory, both preserving this layer's own
//! entries regardless of their order. Other entries replace what was there
//! unless both are directories.

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
    unpack(reader, Path::new("/"))?;
    let left = layer.metadata()?.len() - layer.stream_position()?;
    ensure!(left == 0, "layer has {left} unconsumed bytes after its last frame");
    Ok(())
}

/// Production passes `/` after chroot; tests use a private temporary root.
fn unpack(reader: impl Read, root: &Path) -> Result<()> {
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
        let (dir, full) = (root.join(dir), root.join(&path));
        if name == ".wh..wh..opq" {
            if let Some(dir) = resolved_dir(&dir)? {
                opaque(&dir, &extracted)?;
            }
        } else if let Some(target) = name.as_bytes().strip_prefix(b".wh.") {
            ensure!(!target.is_empty() && target != b"." && target != b"..", "invalid whiteout: {}", path.display());
            if let Some(dir) = resolved_dir(&dir)? {
                remove_lower(&dir.join(OsStr::from_bytes(target)), &extracted)?;
            }
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
                    ensure!(entry.unpack_in(root).with_context(|| format!("extract {}", path.display()))?, "unsafe path in layer: {}", path.display());
                }
            }
            // Where the entry really is (its parent may be a symlink), and its
            // ancestors: whiteouts hide only what lower layers left. Keeping
            // ancestors also preserves directories created implicitly by a file.
            if let Some(dir) = resolved_dir(&dir)? {
                let real = dir.join(name);
                for a in real.ancestors() {
                    if !extracted.insert(a.to_path_buf()) {
                        break;
                    }
                }
            }
        }
    }
    // Whatever follows the tar end-of-archive must still be read, so the
    // rootfs never differs from what another runtime would extract.
    io::copy(&mut archive.into_inner(), &mut io::sink())?;
    Ok(())
}

fn absent(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory)
}

/// Whiteouts of absent paths are harmless; resolution failures are not.
fn resolved_dir(dir: &Path) -> Result<Option<PathBuf>> {
    match fs::canonicalize(dir) {
        Ok(dir) => Ok(Some(dir)),
        Err(e) if absent(&e) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("resolve layer directory {}", dir.display())),
    }
}

/// Removes a lower-layer path, retaining this layer's entries and ancestors.
fn remove_lower(path: &Path, extracted: &HashSet<PathBuf>) -> Result<()> {
    if !extracted.contains(path) {
        return remove(path);
    }
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => opaque(path, extracted),
        Ok(_) => Ok(()),
        Err(e) if absent(&e) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("inspect layer path {}", path.display())),
    }
}

/// Removes everything below `dir` (resolved) that this layer did not extract.
fn opaque(dir: &Path, extracted: &HashSet<PathBuf>) -> Result<()> {
    let children = match fs::read_dir(dir) {
        Ok(children) => children,
        Err(e) if absent(&e) => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("read layer directory {}", dir.display())),
    };
    for child in children {
        remove_lower(&child?.path(), extracted)?;
    }
    Ok(())
}

fn remove(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p)?,
        Ok(_) => fs::remove_file(p)?,
        Err(e) if absent(&e) => {}
        Err(e) => return Err(e).with_context(|| format!("inspect layer path {}", p.display())),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Root(PathBuf);

    impl Root {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = std::env::temp_dir().join(format!("vcontainer-unpack-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("create fixture root: {e}"),
                }
            }
        }

        fn apply(&self, entries: &[Node<'_>]) -> Result<()> {
            let owner = fs::metadata(&self.0)?;
            let mut builder = tar::Builder::new(Vec::new());
            for entry in entries {
                let mut header = tar::Header::new_ustar();
                header.set_uid(owner.uid() as u64);
                header.set_gid(owner.gid() as u64);
                header.set_mtime(0);
                let (name, data): (&str, &[u8]) = match entry {
                    Node::File(name, data) => {
                        header.set_entry_type(EntryType::Regular);
                        header.set_mode(0o644);
                        (*name, *data)
                    }
                    Node::Dir(name) => {
                        header.set_entry_type(EntryType::Directory);
                        header.set_mode(0o755);
                        (*name, &[])
                    }
                    Node::Link(name, target) => {
                        header.set_entry_type(EntryType::Symlink);
                        header.set_link_name(*target)?;
                        header.set_mode(0o777);
                        (*name, &[])
                    }
                };
                header.set_size(data.len() as u64);
                builder.append_data(&mut header, name, data)?;
            }
            unpack(io::Cursor::new(builder.into_inner()?), &self.0)
        }

        fn read(&self, name: &str) -> Vec<u8> {
            fs::read(self.0.join(name)).unwrap()
        }

        fn exists(&self, name: &str) -> bool {
            fs::symlink_metadata(self.0.join(name)).is_ok()
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Clone, Copy)]
    enum Node<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Link(&'a str, &'a str),
    }

    use Node::{Dir, File, Link};

    #[test]
    fn ordinary_whiteout_keeps_same_layer_file_in_either_order() {
        for whiteout_first in [false, true] {
            let root = Root::new();
            root.apply(&[File("etc/policy", b"old")]).unwrap();
            let mut upper = vec![File("etc/policy", b"new"), File("etc/.wh.policy", b"")];
            if whiteout_first {
                upper.reverse();
            }
            root.apply(&upper).unwrap();
            assert_eq!(root.read("etc/policy"), b"new");
        }
    }

    #[test]
    fn ordinary_whiteout_keeps_descendants_but_removes_lower_siblings() {
        for whiteout_first in [false, true] {
            let root = Root::new();
            root.apply(&[File("d/old", b"old"), File("d/sub/old", b"old"), File("d/other/old", b"old")]).unwrap();
            let mut upper = vec![File("d/new", b"new"), File("d/sub/new", b"new"), File(".wh.d", b"")];
            if whiteout_first {
                upper.rotate_right(1);
            }
            root.apply(&upper).unwrap();
            assert_eq!(root.read("d/new"), b"new");
            assert_eq!(root.read("d/sub/new"), b"new");
            for name in ["d/old", "d/sub/old", "d/other"] {
                assert!(!root.exists(name), "retained lower-layer {name}");
            }
        }
    }

    #[test]
    fn ordinary_whiteout_keeps_explicit_empty_same_layer_directory() {
        let root = Root::new();
        root.apply(&[File("d/old", b"old")]).unwrap();
        root.apply(&[Dir("d"), File(".wh.d", b"")]).unwrap();
        assert!(root.0.join("d").is_dir());
        assert_eq!(fs::read_dir(root.0.join("d")).unwrap().count(), 0);
    }

    #[test]
    fn ordinary_whiteout_removes_lower_file_directory_and_symlink() {
        let root = Root::new();
        root.apply(&[File("old", b"old"), File("d/old", b"old"), File("target/old", b"old"), Link("link", "target")]).unwrap();
        root.apply(&[File(".wh.old", b""), File(".wh.d", b""), File(".wh.link", b""), File("missing/.wh.absent", b"")]).unwrap();
        for name in ["old", "d", "link", "missing"] {
            assert!(!root.exists(name), "retained lower-layer {name}");
        }
        assert_eq!(root.read("target/old"), b"old");
    }

    #[test]
    fn ordinary_and_opaque_whiteouts_preserve_same_layer_contents() {
        let upper = [File("d/new", b"new"), File("d/sub/new", b"new"), File(".wh.d", b""), File("d/.wh..wh..opq", b"")];
        for order in [[0, 1, 2, 3], [0, 1, 3, 2], [2, 3, 0, 1], [3, 2, 0, 1], [0, 2, 1, 3], [0, 3, 1, 2]] {
            let root = Root::new();
            root.apply(&[File("d/old", b"old"), File("d/sub/old", b"old")]).unwrap();
            root.apply(&order.map(|i| upper[i])).unwrap();
            assert_eq!(root.read("d/new"), b"new");
            assert_eq!(root.read("d/sub/new"), b"new");
            assert!(!root.exists("d/old"));
            assert!(!root.exists("d/sub/old"));
            assert!(!root.exists(".wh.d"));
            assert!(!root.exists("d/.wh..wh..opq"));
        }
    }

    #[test]
    fn whiteouts_resolve_parent_symlinks_and_preserve_same_layer_symlink() {
        let root = Root::new();
        root.apply(&[File("target/old", b"old"), Link("alias", "target")]).unwrap();
        root.apply(&[File("alias/new", b"new"), File("alias/.wh.new", b""), File("alias/.wh.old", b""), Link("own-link", "target"), File(".wh.own-link", b"")]).unwrap();
        assert_eq!(root.read("target/new"), b"new");
        assert!(!root.exists("target/old"));
        assert_eq!(fs::read_link(root.0.join("own-link")).unwrap(), Path::new("target"));
    }

    #[test]
    fn whiteouts_reject_empty_dot_and_parent_targets() {
        for marker in [".wh.", ".wh..", ".wh..."] {
            let root = Root::new();
            root.apply(&[File("keep", b"keep")]).unwrap();
            assert!(root.apply(&[File(marker, b"")]).is_err(), "accepted {marker}");
            assert_eq!(root.read("keep"), b"keep");
        }
    }

    #[test]
    fn whiteouts_propagate_parent_resolution_errors() {
        for marker in ["loop/.wh.file", "loop/.wh..wh..opq"] {
            let root = Root::new();
            root.apply(&[Link("loop", "loop")]).unwrap();
            assert!(root.apply(&[File(marker, b"")]).is_err(), "ignored symlink loop for {marker}");
        }
    }
}
