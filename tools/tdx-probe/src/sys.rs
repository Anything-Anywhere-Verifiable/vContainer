//! The few libc calls std does not wrap. Declared by hand against musl so the
//! crate has no dependencies.

use std::ffi::CString;
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use std::os::raw::{c_char, c_int, c_ulong, c_void};

extern "C" {
    fn mount(src: *const c_char, target: *const c_char, fstype: *const c_char, flags: c_ulong, data: *const c_void) -> c_int;
    fn klogctl(kind: c_int, buf: *mut c_char, len: c_int) -> c_int;
    fn ioctl(fd: c_int, req: c_int, ...) -> c_int; // musl: int ioctl(int, int, ...)
    fn reboot(cmd: c_int) -> c_int;
    fn sync();
}

pub const MS_NOSUID: c_ulong = 2;
pub const MS_NODEV: c_ulong = 4;
pub const MS_NOEXEC: c_ulong = 8;

fn check(rc: c_int) -> io::Result<c_int> {
    if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(rc) }
}

pub fn do_mount(src: &str, target: &str, fstype: &str, flags: c_ulong, data: &str) -> io::Result<()> {
    let (s, t, f, d) = (CString::new(src)?, CString::new(target)?, CString::new(fstype)?, CString::new(data)?);
    check(unsafe { mount(s.as_ptr(), t.as_ptr(), f.as_ptr(), flags, d.as_ptr().cast()) }).map(|_| ())
}

/// The whole kernel log buffer (SYSLOG_ACTION_READ_ALL), like `dmesg`.
pub fn kmsg_read_all() -> io::Result<String> {
    const SIZE_BUFFER: c_int = 10;
    const READ_ALL: c_int = 3;
    let size = check(unsafe { klogctl(SIZE_BUFFER, std::ptr::null_mut(), 0) })?.max(1 << 14);
    let mut buf = vec![0u8; size as usize];
    let n = check(unsafe { klogctl(READ_ALL, buf.as_mut_ptr().cast(), size) })?;
    buf.truncate(n as usize);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// SYSLOG_ACTION_CONSOLE_LEVEL: only messages more urgent than `level` reach ttyS0.
pub fn console_level(level: c_int) -> io::Result<()> {
    check(unsafe { klogctl(8, std::ptr::null_mut(), level) }).map(|_| ())
}

fn sockaddr_in(a: Ipv4Addr) -> [u8; 16] {
    let mut s = [0u8; 16];
    s[..2].copy_from_slice(&2u16.to_ne_bytes()); // AF_INET
    s[4..8].copy_from_slice(&a.octets());
    s
}

/// struct rtentry from <net/route.h> (x86_64 layout via repr(C)).
#[repr(C)]
struct RtEntry {
    rt_pad1: c_ulong,
    rt_dst: [u8; 16],
    rt_gateway: [u8; 16],
    rt_genmask: [u8; 16],
    rt_flags: u16,
    rt_pad2: i16,
    rt_pad3: c_ulong,
    rt_pad4: *mut c_void,
    rt_metric: i16,
    rt_dev: *mut c_char,
    rt_mtu: c_ulong,
    rt_window: c_ulong,
    rt_irtt: u16,
}

pub const RTF_UP: u16 = 0x1;
pub const RTF_GATEWAY: u16 = 0x2;
pub const RTF_HOST: u16 = 0x4;

/// ioctl(SIOCADDRT): add an IPv4 route, like `route add`.
pub fn add_route(dst: Ipv4Addr, mask: Ipv4Addr, gw: Ipv4Addr, flags: u16, dev: &str) -> io::Result<()> {
    const SIOCADDRT: c_int = 0x890B;
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    let dev = CString::new(dev)?;
    let mut rt = RtEntry {
        rt_pad1: 0,
        rt_dst: sockaddr_in(dst),
        rt_gateway: sockaddr_in(gw),
        rt_genmask: sockaddr_in(mask),
        rt_flags: flags,
        rt_pad2: 0,
        rt_pad3: 0,
        rt_pad4: std::ptr::null_mut(),
        rt_metric: 0,
        rt_dev: dev.as_ptr() as *mut c_char,
        rt_mtu: 0,
        rt_window: 0,
        rt_irtt: 0,
    };
    check(unsafe { ioctl(sock.as_raw_fd(), SIOCADDRT, &mut rt as *mut RtEntry) }).map(|_| ())
}

pub fn power_off() -> io::Error {
    const RB_POWER_OFF: c_int = 0x4321fedc;
    unsafe {
        sync();
        reboot(RB_POWER_OFF);
    }
    io::Error::last_os_error()
}
