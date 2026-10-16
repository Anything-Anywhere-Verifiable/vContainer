//! Networking. The kernel's `ip=dhcp` (built into the measured command line)
//! configures the address, MTU and DNS servers, but GCE leases a /32 with an
//! off-link gateway, so the kernel cannot add the default route itself. It
//! does log the gateway (`IP-Config: Complete ... gw=`); we add the routes.

use anyhow::{bail, ensure, Context, Result};
use std::fs;
use std::net::Ipv4Addr;
use std::time::Duration;

use crate::sys::{check, cstr};

pub fn setup() -> Result<()> {
    let log = kernel_log()?;
    // The host answers the hypervisor CPUID leaves; only KVM's paravirtual
    // paths are part of the trusted default configuration.
    ensure!(log.contains("Hypervisor detected: KVM"), "the kernel did not detect KVM as the hypervisor");
    let line = log
        .lines()
        .rfind(|l| l.contains("device=") && l.contains(" gw="))
        .context("kernel IP autoconfiguration did not complete (no IP-Config line)")?;
    let dev = field(line, "device=")?;
    let gw: Ipv4Addr = field(line, "gw=")?.parse().context("bad gateway in IP-Config line")?;
    ensure!(!gw.is_unspecified(), "DHCP lease has no gateway");
    route(&dev, gw, None).context("add host route to gateway")?;
    route(&dev, Ipv4Addr::UNSPECIFIED, Some(gw)).context("add default route")?;
    // /proc/net/pnp is resolv.conf syntax (nameserver/domain lines).
    let pnp = fs::read_to_string("/proc/net/pnp").unwrap_or_default();
    let resolv: String = pnp
        .lines()
        .filter(|l| l.starts_with("nameserver") || l.starts_with("domain"))
        .map(|l| format!("{l}\n"))
        .collect();
    ensure!(resolv.contains("nameserver"), "DHCP lease has no DNS server");
    fs::create_dir_all("/etc")?;
    fs::write("/etc/resolv.conf", &resolv)?;
    // Kernel DHCP set the host-chosen name; use a constant one.
    let name = b"vcontainer";
    check(unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) }, "sethostname")?;
    crate::sys::log(&format!("network: {dev} via {gw}"));
    Ok(())
}

fn field(line: &str, key: &str) -> Result<String> {
    let start = line.find(key).context("missing field")? + key.len();
    Ok(line[start..].split([',', ' ']).next().unwrap_or("").to_string())
}

fn kernel_log() -> Result<String> {
    const READ_ALL: libc::c_int = 3;
    const SIZE_BUFFER: libc::c_int = 10;
    let size = unsafe { libc::klogctl(SIZE_BUFFER, std::ptr::null_mut(), 0) };
    check(size, "klogctl size")?;
    let mut buf = vec![0u8; size as usize];
    let n = unsafe { libc::klogctl(READ_ALL, buf.as_mut_ptr().cast(), buf.len() as libc::c_int) };
    check(n, "klogctl read")?;
    buf.truncate(n as usize);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn sockaddr(ip: Ipv4Addr) -> libc::sockaddr {
    let sin = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr { s_addr: u32::from(ip).to_be() },
        sin_zero: [0; 8],
    };
    unsafe { std::mem::transmute(sin) }
}

/// Adds a host route to `dst` on `dev` (gw = None) or a route via `gw`.
fn route(dev: &str, dst: Ipv4Addr, gw: Option<Ipv4Addr>) -> Result<()> {
    let devname = cstr(dev);
    let mut rt: libc::rtentry = unsafe { std::mem::zeroed() };
    rt.rt_dst = sockaddr(dst);
    rt.rt_dev = devname.as_ptr() as *mut libc::c_char;
    match gw {
        None => {
            rt.rt_genmask = sockaddr(Ipv4Addr::BROADCAST);
            rt.rt_flags = libc::RTF_UP | libc::RTF_HOST;
        }
        Some(gw) => {
            rt.rt_genmask = sockaddr(Ipv4Addr::UNSPECIFIED);
            rt.rt_gateway = sockaddr(gw);
            rt.rt_flags = libc::RTF_UP | libc::RTF_GATEWAY;
        }
    }
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    check(sock, "socket")?;
    let r = unsafe { libc::ioctl(sock, libc::SIOCADDRT as _, &rt) };
    let exists = r < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST);
    unsafe { libc::close(sock) };
    // In 10/8 the kernel guesses a classful netmask and may already have
    // added the route itself.
    if exists {
        return Ok(());
    }
    check(r, "SIOCADDRT")
}

/// Reads a GCE metadata value (raw bytes). The metadata server is untrusted:
/// callers measure what they read before using it.
pub fn metadata(path: &str) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into();
    let url = format!("http://169.254.169.254/computeMetadata/v1/{path}");
    for attempt in 0..30 {
        match agent.get(&url).header("Metadata-Flavor", "Google").call() {
            Ok(mut r) if r.status() == 200 => return Ok(r.body_mut().with_config().limit(64 << 10).read_to_vec()?),
            Ok(r) if r.status() == 404 => bail!("metadata {path} is not set"),
            other => {
                if attempt == 29 {
                    bail!("metadata {path}: {:?}", other.map(|r| r.status()));
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    unreachable!()
}
