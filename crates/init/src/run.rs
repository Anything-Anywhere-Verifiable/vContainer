//! Container start helper (`/init --stage run`, spec on stdin).
//!
//! The helper is single-threaded, so it can unshare and fork safely. Its
//! child becomes PID 1 of a new PID namespace, gets a private mount
//! namespace without any of /init's mounts (configfs, writable sysfs, TEE
//! devices), a read-only /proc/sys, Docker's default capabilities minus
//! MKNOD and SYS_CHROOT, and then execs the workload. A CLOEXEC pipe tells
//! the helper whether exec happened; the helper reports on stdout:
//! `started`, `failed <reason>`, then `exited <code>`.

use anyhow::{bail, Context, Result};
use caps::{CapSet, Capability};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::FromRawFd;
use std::path::Path;

use crate::deploy::ROOT;
use crate::sys::{bind_readonly, check, cstr, mount, umount_detach};

#[derive(Serialize, Deserialize)]
pub struct RunSpec {
    pub argv: Vec<String>,
    pub env: Vec<String>,
    pub cwd: String,
    pub uid: u32,
    pub gid: u32,
    pub serial: bool,
}

/// Docker's default capability set without MKNOD and SYS_CHROOT.
const CAPS: [Capability; 12] = [
    Capability::CAP_CHOWN,
    Capability::CAP_DAC_OVERRIDE,
    Capability::CAP_FSETID,
    Capability::CAP_FOWNER,
    Capability::CAP_NET_RAW,
    Capability::CAP_SETGID,
    Capability::CAP_SETUID,
    Capability::CAP_SETFCAP,
    Capability::CAP_SETPCAP,
    Capability::CAP_NET_BIND_SERVICE,
    Capability::CAP_KILL,
    Capability::CAP_AUDIT_WRITE,
];

pub fn main() -> Result<()> {
    let spec: RunSpec = serde_json::from_reader(std::io::stdin())?;
    let mut fds = [0; 2];
    check(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, "pipe")?;
    check(unsafe { libc::unshare(libc::CLONE_NEWNS | libc::CLONE_NEWPID) }, "unshare")?;
    let pid = unsafe { libc::fork() };
    check(pid, "fork")?;
    if pid == 0 {
        unsafe { libc::close(fds[0]) };
        let err = format!("{:#}", setup_and_exec(&spec).unwrap_err());
        unsafe {
            libc::write(fds[1], err.as_ptr().cast(), err.len());
            libc::_exit(127);
        }
    }
    unsafe { libc::close(fds[1]) };
    let mut why = String::new();
    unsafe { File::from_raw_fd(fds[0]) }.read_to_string(&mut why)?;
    let mut out = std::io::stdout();
    if why.is_empty() {
        writeln!(out, "started")?;
    } else {
        writeln!(out, "failed {}", why.replace('\n', " "))?;
    }
    out.flush()?;
    let mut st = 0;
    check(unsafe { libc::waitpid(pid, &mut st, 0) }, "waitpid")?;
    let code = if libc::WIFEXITED(st) { libc::WEXITSTATUS(st) } else { 128 + libc::WTERMSIG(st) };
    writeln!(out, "exited {code}")?;
    Ok(())
}

fn setup_and_exec(spec: &RunSpec) -> Result<std::convert::Infallible> {
    // stdio: stdin is never the host-controlled console.
    let null = unsafe { libc::open(cstr("/dev/null").as_ptr(), libc::O_RDWR) };
    let out = if spec.serial { unsafe { libc::open(cstr("/dev/console").as_ptr(), libc::O_WRONLY) } } else { null };
    check(null.min(out), "open stdio")?;
    for (fd, to) in [(null, 0), (out, 1), (out, 2)] {
        check(unsafe { libc::dup2(fd, to) }, "dup2")?;
    }

    mount("", "/", "", libc::MS_REC | libc::MS_PRIVATE, None).context("make mounts private")?;
    let r = |p: &str| format!("{ROOT}{p}");
    mount("proc", &r("/proc"), "proc", libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC, None)?;
    for p in ["/proc/sys", "/proc/sysrq-trigger", "/proc/irq", "/proc/bus", "/proc/fs"] {
        if Path::new(&r(p)).exists() {
            bind_readonly(&r(p))?;
        }
    }
    for p in ["/proc/kcore", "/proc/keys", "/proc/timer_list"] {
        if Path::new(&r(p)).exists() {
            mount("/dev/null", &r(p), "", libc::MS_BIND, None)?;
        }
    }
    mount("sysfs", &r("/sys"), "sysfs", libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC, None)?;
    mount("tmpfs", &r("/dev"), "tmpfs", libc::MS_NOSUID, Some("mode=755,size=65536k"))?;
    for (name, minor_major) in [("null", (1, 3)), ("zero", (1, 5)), ("full", (1, 7)), ("random", (1, 8)), ("urandom", (1, 9)), ("tty", (5, 0))] {
        let dev = libc::makedev(minor_major.0, minor_major.1);
        check(unsafe { libc::mknod(cstr(&r(&format!("/dev/{name}"))).as_ptr(), libc::S_IFCHR | 0o666, dev) }, "mknod")?;
        std::fs::set_permissions(r(&format!("/dev/{name}")), std::fs::Permissions::from_mode(0o666))?;
    }
    mount("devpts", &r("/dev/pts"), "devpts", libc::MS_NOSUID | libc::MS_NOEXEC, Some("newinstance,ptmxmode=0666,mode=0620"))?;
    std::os::unix::fs::symlink("pts/ptmx", r("/dev/ptmx"))?;
    mount("shm", &r("/dev/shm"), "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, Some("mode=1777"))?;
    for (link, target) in [("fd", "/proc/self/fd"), ("stdin", "/proc/self/fd/0"), ("stdout", "/proc/self/fd/1"), ("stderr", "/proc/self/fd/2")] {
        std::os::unix::fs::symlink(target, r(&format!("/dev/{link}")))?;
    }

    // Drop every mount /init uses, including configfs and securityfs.
    for p in ["/proc", "/sys", "/dev", "/run"] {
        if let Err(e) = umount_detach(p) {
            // EINVAL: not a mount point here (e.g. in tests); anything else is fatal.
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL) {
                return Err(e);
            }
        }
    }
    std::env::set_current_dir(ROOT)?;
    mount(".", "/", "", libc::MS_MOVE, None).context("move new root")?;
    check(unsafe { libc::chroot(cstr(".").as_ptr()) }, "chroot")?;
    std::env::set_current_dir("/")?;

    let keep: HashSet<Capability> = CAPS.into_iter().collect();
    for cap in caps::all() {
        if !keep.contains(&cap) {
            caps::drop(None, CapSet::Bounding, cap).context("drop bounding capability")?;
        }
    }
    let gid = spec.gid as libc::gid_t;
    check(unsafe { libc::setgroups(1, &gid) }, "setgroups")?;
    check(unsafe { libc::setgid(gid) }, "setgid")?;
    check(unsafe { libc::setuid(spec.uid) }, "setuid")?;
    if spec.uid == 0 {
        caps::set(None, CapSet::Effective, &keep)?;
        caps::set(None, CapSet::Permitted, &keep)?;
        caps::set(None, CapSet::Inheritable, &HashSet::new())?;
    }
    caps::clear(None, CapSet::Ambient)?;

    std::fs::create_dir_all(&spec.cwd)?;
    std::env::set_current_dir(&spec.cwd).with_context(|| format!("chdir {}", spec.cwd))?;
    // Mark every other descriptor close-on-exec (the status pipe already is).
    unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32 /* CLOSE_RANGE_CLOEXEC */) };

    let prog = resolve(&spec.argv[0], &spec.env)?;
    let c = |v: &[String]| v.iter().map(|s| CString::new(s.as_str())).collect::<Result<Vec<_>, _>>();
    let (argv, env) = (c(&spec.argv)?, c(&spec.env)?);
    let mut pa: Vec<*const libc::c_char> = argv.iter().map(|s| s.as_ptr()).collect();
    let mut pe: Vec<*const libc::c_char> = env.iter().map(|s| s.as_ptr()).collect();
    pa.push(std::ptr::null());
    pe.push(std::ptr::null());
    unsafe { libc::execve(cstr(&prog).as_ptr(), pa.as_ptr(), pe.as_ptr()) };
    bail!("exec {prog}: {}", std::io::Error::last_os_error())
}

/// Finds `prog` on the container's PATH, as execvp would.
fn resolve(prog: &str, env: &[String]) -> Result<String> {
    if prog.contains('/') {
        return Ok(prog.to_string());
    }
    let path = env.iter().find_map(|e| e.strip_prefix("PATH=")).unwrap_or("/usr/bin:/bin");
    for dir in path.split(':') {
        let cand = format!("{dir}/{prog}");
        if std::fs::metadata(&cand).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
            return Ok(cand);
        }
    }
    bail!("{prog}: not found on PATH {path}")
}
