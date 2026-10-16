//! Thin wrappers around the few syscalls PID 1 needs.

use anyhow::{bail, Result};
use std::ffi::CString;
use std::io;
use std::ptr::null;

pub fn cstr(s: &str) -> CString {
    CString::new(s).expect("no NUL in path")
}

pub fn check(ret: libc::c_int, what: &str) -> Result<()> {
    if ret < 0 {
        bail!("{what}: {}", io::Error::last_os_error());
    }
    Ok(())
}

pub fn mount(src: &str, target: &str, fstype: &str, flags: libc::c_ulong, data: Option<&str>) -> Result<()> {
    let _ = std::fs::create_dir_all(target);
    let (s, t, f) = (cstr(src), cstr(target), cstr(fstype));
    let d = data.map(cstr);
    let r = unsafe {
        libc::mount(
            s.as_ptr(),
            t.as_ptr(),
            if fstype.is_empty() { null() } else { f.as_ptr() },
            flags,
            d.as_ref().map_or(null(), |d| d.as_ptr().cast()),
        )
    };
    check(r, &format!("mount {fstype} on {target}"))
}

/// Bind-mounts `path` onto itself read-only.
pub fn bind_readonly(path: &str) -> Result<()> {
    mount(path, path, "", libc::MS_BIND | libc::MS_REC, None)?;
    mount(path, path, "", libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC, None)
}

pub fn umount_detach(target: &str) -> Result<()> {
    check(unsafe { libc::umount2(cstr(target).as_ptr(), libc::MNT_DETACH) }, &format!("umount {target}"))
}

pub fn log(msg: &str) {
    eprintln!("vcontainer: {msg}");
}

pub fn poweroff() -> ! {
    unsafe {
        libc::sync();
        libc::reboot(libc::RB_POWER_OFF);
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

/// Reaps every child forever; PID 1 never needs exit statuses (helpers
/// report over their stdout).
pub fn reap_forever() -> ! {
    loop {
        let mut status = 0;
        if unsafe { libc::waitpid(-1, &mut status, 0) } < 0 {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

pub fn random<const N: usize>() -> [u8; N] {
    use rand_core::RngCore;
    let mut b = [0u8; N];
    rand_core::OsRng.fill_bytes(&mut b);
    b
}
