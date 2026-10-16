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
use std::collections::{BTreeMap, HashSet};
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::FromRawFd;
use std::path::Path;

use crate::deploy::ROOT;
use crate::sys::{bind_readonly, check, cstr, mount, umount_detach};

#[derive(Serialize, Deserialize)]
pub struct RunSpec {
    pub argv: Vec<String>,
    pub env: Vec<String>,
    pub cwd: String,
    /// `user[:group]` from the launch config or image (empty: root).
    pub user: String,
    /// Sealed secret files (base64), written to /run/secrets.
    pub files: BTreeMap<String, String>,
    pub resolv: String,
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
    mount("shm", &r("/dev/shm"), "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, Some("mode=1777,size=65536k"))?;
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
    // From here on every path, and every symlink in the image, resolves
    // inside the container's root.
    let (uid, gid, home) = prepare(spec)?;
    let env = with_defaults(spec.env.clone(), &home);

    // Docker's default open-file limits.
    let nofile = libc::rlimit { rlim_cur: 1024, rlim_max: 524288 };
    check(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &nofile) }, "setrlimit")?;
    let keep: HashSet<Capability> = CAPS.into_iter().collect();
    for cap in caps::all() {
        if !keep.contains(&cap) {
            caps::drop(None, CapSet::Bounding, cap).context("drop bounding capability")?;
        }
    }
    check(unsafe { libc::setgroups(1, &gid) }, "setgroups")?;
    check(unsafe { libc::setgid(gid) }, "setgid")?;
    check(unsafe { libc::setuid(uid) }, "setuid")?;
    if uid == 0 {
        caps::set(None, CapSet::Effective, &keep)?;
        caps::set(None, CapSet::Permitted, &keep)?;
        caps::set(None, CapSet::Inheritable, &HashSet::new())?;
    }
    caps::clear(None, CapSet::Ambient)?;

    std::fs::create_dir_all(&spec.cwd)?;
    std::env::set_current_dir(&spec.cwd).with_context(|| format!("chdir {}", spec.cwd))?;
    // Mark every other descriptor close-on-exec (the status pipe already is).
    unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32 /* CLOSE_RANGE_CLOEXEC */) };

    let prog = resolve(&spec.argv[0], &env)?;
    let c = |v: &[String]| v.iter().map(|s| CString::new(s.as_str())).collect::<Result<Vec<_>, _>>();
    let (argv, env) = (c(&spec.argv)?, c(&env)?);
    let mut pa: Vec<*const libc::c_char> = argv.iter().map(|s| s.as_ptr()).collect();
    let mut pe: Vec<*const libc::c_char> = env.iter().map(|s| s.as_ptr()).collect();
    pa.push(std::ptr::null());
    pe.push(std::ptr::null());
    // Rust ignores SIGPIPE; the workload gets the default, as under Docker.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    unsafe { libc::execve(cstr(&prog).as_ptr(), pa.as_ptr(), pe.as_ptr()) };
    bail!("exec {prog}: {}", std::io::Error::last_os_error())
}

/// Writes /etc files and secrets (as root, inside the new root) and resolves
/// the user. Returns (uid, gid, home).
fn prepare(spec: &RunSpec) -> Result<(u32, u32, String)> {
    std::fs::create_dir_all("/etc")?;
    write_new("/etc/resolv.conf", spec.resolv.as_bytes(), 0o644)?;
    write_new("/etc/hosts", b"127.0.0.1\tlocalhost vcontainer\n", 0o644)?;
    write_new("/etc/hostname", b"vcontainer\n", 0o644)?;
    let (uid, gid, home) = user(&spec.user)?;
    if !spec.files.is_empty() {
        std::fs::create_dir_all("/run/secrets")?;
        for (name, b64) in &spec.files {
            let path = format!("/run/secrets/{name}");
            write_new(&path, &vcontainer_core::b64::decode(b64)?, 0o400)?;
            std::os::unix::fs::chown(&path, Some(uid), Some(gid))?;
        }
    }
    Ok((uid, gid, home))
}

/// Replaces `path` with a new file, never following a symlink at `path`.
fn write_new(path: &str, data: &[u8], mode: u32) -> Result<()> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir()) {
        std::fs::remove_file(path)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("create {path}"))?;
    f.write_all(data)?;
    Ok(())
}

/// Resolves `user[:group]` against the image's /etc/passwd and /etc/group.
fn user(spec: &str) -> Result<(u32, u32, String)> {
    let read = |f: &str| std::fs::read_to_string(f).unwrap_or_default();
    let (passwd, group) = (read("/etc/passwd"), read("/etc/group"));
    let (u, g) = spec.split_once(':').map_or((spec, None), |(u, g)| (u, Some(g)));
    let entry: Option<Vec<&str>> = passwd.lines().map(|l| l.split(':').collect::<Vec<_>>()).find(|f| {
        f.len() >= 6 && (f[0] == u || f[2] == u || (u.is_empty() && f[2] == "0"))
    });
    let uid = match (u.parse::<u32>(), &entry) {
        (Ok(n), _) => n,
        (_, Some(f)) => f[2].parse()?,
        _ if u.is_empty() => 0,
        _ => bail!("user {u:?} is not in the image's /etc/passwd"),
    };
    let gid = match g {
        Some(g) => match g.parse::<u32>() {
            Ok(n) => n,
            Err(_) => group
                .lines()
                .find_map(|l| {
                    let f: Vec<&str> = l.split(':').collect();
                    (f.len() >= 3 && f[0] == g).then(|| f[2].parse().ok()).flatten()
                })
                .with_context(|| format!("group {g:?} is not in the image's /etc/group"))?,
        },
        None => entry.as_ref().and_then(|f| f[3].parse().ok()).unwrap_or(0),
    };
    let home = entry.as_ref().map(|f| f[5].to_string()).unwrap_or_else(|| "/".into());
    Ok((uid, gid, home))
}

fn with_defaults(mut env: Vec<String>, home: &str) -> Vec<String> {
    for (k, v) in [("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"), ("HOME", home), ("HOSTNAME", "vcontainer")] {
        if !env.iter().any(|e| e.starts_with(&format!("{k}="))) {
            env.push(format!("{k}={v}"));
        }
    }
    env
}

/// Finds `prog` on the container's PATH, as execvp would.
fn resolve(prog: &str, env: &[String]) -> Result<String> {
    if prog.contains('/') {
        return Ok(prog.to_string());
    }
    let path = env.iter().find_map(|e| e.strip_prefix("PATH=")).unwrap_or("/usr/bin:/bin");
    for dir in path.split(':') {
        let cand = Path::new(dir).join(prog).to_string_lossy().into_owned();
        if std::fs::metadata(&cand).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
            return Ok(cand);
        }
    }
    bail!("{prog}: not found on PATH {path}")
}
