//! tdx-probe: a throwaway /init for the M0 boot of the vcontainer image on a
//! GCE C3 TDX VM. It prints everything to ttyS0 (read back with
//! `gcloud compute instances get-serial-port-output`), never stops on an
//! error, prints `PROBE RESULT PASS/FAIL` and `PROBE DONE`, then powers off.

mod sha;
mod sys;
mod tcg;

use sha::{base64, hex, sha384, sha512};
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

const MEAS: &str = "/sys/devices/virtual/misc/tdx_guest/measurements";
const TSM: &str = "/sys/kernel/config/tsm/report/probe";
const METADATA: &str = "169.254.169.254:80";
const PROBE_STRING: &[u8] = b"vcontainer-probe";

type R = Result<(), String>;

#[derive(Default)]
struct State {
    failures: usize,
    kmsg_len: usize,
    device: Option<String>,
    gw: Option<Ipv4Addr>,
    instance_id: Option<String>,
    project_id: Option<String>,
    zone: Option<String>,
    mrs: Vec<(String, Vec<u8>)>,
}

impl State {
    fn mr(&self, name: &str) -> Option<&[u8]> {
        self.mrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_slice())
    }
}

fn e<E: std::fmt::Display>(ctx: &str) -> impl FnOnce(E) -> String + '_ {
    move |err| format!("{ctx}: {err}")
}

/// Run one check; an error or a panic is printed and the probe goes on.
fn step(st: &mut State, name: &str, f: fn(&mut State) -> R) {
    println!("--- {name}");
    match catch_unwind(AssertUnwindSafe(|| f(st))) {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => {
            st.failures += 1;
            println!("ERR {name}: {msg}");
        }
        Err(_) => {
            st.failures += 1;
            println!("ERR {name}: panicked");
        }
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| println!("PANIC: {info}")));
    println!("=== vcontainer tdx-probe {} ===", env!("CARGO_PKG_VERSION"));
    let mut st = State::default();
    step(&mut st, "mount", mounts);
    step(&mut st, "uname", uname);
    step(&mut st, "kmsg ip-config", kmsg_ipconfig);
    step(&mut st, "/proc/net/pnp (before)", |_| print_file("PNP", "/proc/net/pnp"));
    step(&mut st, "routes", routes);
    step(&mut st, "/proc/net/route", |_| print_file("ROUTE", "/proc/net/route"));
    step(&mut st, "metadata", metadata);
    step(&mut st, "dns+internet", internet);
    step(&mut st, "/proc/net/pnp (after)", |_| print_file("PNP", "/proc/net/pnp"));
    step(&mut st, "loopback", loopback);
    // Keep late kernel messages from interleaving with the long base64
    // lines below; they are printed at the end from the log buffer instead.
    step(&mut st, "console loglevel", |_| sys::console_level(1).map_err(e("klogctl")));
    step(&mut st, "measurements", measurements);
    step(&mut st, "ccel", ccel);
    step(&mut st, "quote", quote);
    step(&mut st, "pzid", pzid);
    step(&mut st, "rtmr3 extend", rtmr3);
    step(&mut st, "kmsg tail", kmsg_tail);
    println!("PROBE RESULT {}", if st.failures == 0 { "PASS" } else { "FAIL" });
    println!("PROBE DONE");
    std::thread::sleep(Duration::from_secs(120));
    println!("powering off");
    let err = sys::power_off();
    println!("ERR power off: {err}");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn mounts(_: &mut State) -> R {
    use sys::{MS_NODEV, MS_NOEXEC, MS_NOSUID};
    let all = [
        ("proc", "/proc", "proc", MS_NOSUID | MS_NODEV | MS_NOEXEC, ""),
        ("sysfs", "/sys", "sysfs", MS_NOSUID | MS_NODEV | MS_NOEXEC, ""),
        ("devtmpfs", "/dev", "devtmpfs", MS_NOSUID | MS_NOEXEC, "mode=0755"),
        ("configfs", "/sys/kernel/config", "configfs", MS_NOSUID | MS_NODEV | MS_NOEXEC, ""),
    ];
    let mut failed = vec![];
    for (src, target, fstype, flags, data) in all {
        match sys::do_mount(src, target, fstype, flags, data) {
            Ok(()) => println!("mounted {fstype} on {target}"),
            Err(err) => failed.push(format!("{target}: {err}")),
        }
    }
    if failed.is_empty() { Ok(()) } else { Err(failed.join("; ")) }
}

fn uname(_: &mut State) -> R {
    let r = fs::read_to_string("/proc/sys/kernel/osrelease").map_err(e("osrelease"))?;
    println!("UNAME: {}", r.trim());
    print_file("VERSION", "/proc/version")?;
    print_file("CMDLINE", "/proc/cmdline")
}

fn print_file(tag: &str, path: &str) -> R {
    let s = fs::read_to_string(path).map_err(e(path))?;
    for l in s.lines() {
        println!("{tag}: {l}");
    }
    Ok(())
}

/// Print the IP-Config lines of the kernel log and take device= and gw=
/// from the "IP-Config: Complete:" block.
fn kmsg_ipconfig(st: &mut State) -> R {
    let log = sys::kmsg_read_all().map_err(e("klogctl"))?;
    st.kmsg_len = log.len();
    let lines: Vec<&str> = log.lines().collect();
    let mut complete = None;
    for (i, l) in lines.iter().enumerate() {
        if l.contains("IP-Config") {
            println!("IPCFG: {l}");
        }
        if l.contains("IP-Config: Complete:") {
            complete = Some(i);
        }
    }
    for l in lines.iter().filter(|l| ["tdx", "TDX", "gve ", "CCEL", "Memory Encryption", "unaccepted"].iter().any(|k| l.contains(k))) {
        println!("KLOG: {l}");
    }
    let i = complete.ok_or("no 'IP-Config: Complete:' in the kernel log")?;
    for l in &lines[i + 1..(i + 5).min(lines.len())] {
        println!("IPCFG: {l}");
    }
    let dev_line = lines[i + 1..].iter().find(|l| l.contains("device=")).ok_or("no device= line")?;
    let field = |k: &str| {
        dev_line.split([',', ' ']).find_map(|w| w.strip_prefix(k)).map(str::to_string)
    };
    st.device = field("device=");
    st.gw = field("gw=").and_then(|g| g.parse().ok());
    println!("PARSED: device={:?} gw={:?}", st.device, st.gw);
    if st.device.is_none() || st.gw.is_none() {
        return Err("could not parse device= / gw=".into());
    }
    Ok(())
}

fn routes(st: &mut State) -> R {
    let (Some(dev), Some(gw)) = (st.device.clone(), st.gw) else {
        return Err("no device/gateway from IP-Config".into());
    };
    let any = Ipv4Addr::UNSPECIFIED;
    let host = sys::add_route(gw, Ipv4Addr::BROADCAST, any, sys::RTF_UP | sys::RTF_HOST, &dev);
    println!("ROUTE ADD {gw}/32 dev {dev}: {}", host.as_ref().map(|_| "OK".to_string()).unwrap_or_else(|x| x.to_string()));
    let def = sys::add_route(any, any, gw, sys::RTF_UP | sys::RTF_GATEWAY, &dev);
    println!("ROUTE ADD default via {gw} dev {dev}: {}", def.as_ref().map(|_| "OK".to_string()).unwrap_or_else(|x| x.to_string()));
    for (name, result) in [("host route", host), ("default route", def)] {
        if let Err(err) = result {
            if err.raw_os_error() != Some(17) { // EEXIST: kernel DHCP installed it.
                return Err(format!("{name}: {err}"));
            }
        }
    }
    Ok(())
}

/// Plain HTTP/1.0 GET; returns (status line, body).
fn http_get(addr: SocketAddr, host: &str, path: &str, extra: &str) -> Result<(String, String), String> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(e("connect"))?;
    s.set_read_timeout(Some(Duration::from_secs(10))).map_err(e("timeout"))?;
    write!(s, "GET {path} HTTP/1.0\r\nHost: {host}\r\n{extra}\r\n").map_err(e("write"))?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(e("read"))?;
    let resp = String::from_utf8_lossy(&resp);
    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((&resp, ""));
    Ok((head.lines().next().unwrap_or("").to_string(), body.to_string()))
}

fn metadata(st: &mut State) -> R {
    let addr: SocketAddr = METADATA.parse().unwrap();
    let mut failed = 0;
    for path in ["instance/id", "project/numeric-project-id", "instance/zone"] {
        let r = http_get(addr, "metadata.google.internal", &format!("/computeMetadata/v1/{path}"), "Metadata-Flavor: Google\r\n");
        match r {
            Ok((status, body)) => {
                println!("META {path}: [{status}] {body}");
                if status.split_whitespace().nth(1) != Some("200") {
                    failed += 1;
                    continue;
                }
                let v = Some(body.trim().to_string());
                match path {
                    "instance/id" => st.instance_id = v,
                    "project/numeric-project-id" => st.project_id = v,
                    _ => st.zone = v,
                }
            }
            Err(err) => {
                println!("META {path}: ERROR {err}");
                failed += 1;
            }
        }
    }
    if failed == 0 { Ok(()) } else { Err(format!("{failed} metadata requests failed")) }
}

/// /proc/net/pnp is already in resolv.conf syntax; install it, then try an
/// IP-literal TCP connect and a DNS-resolved HTTP request.
fn internet(_: &mut State) -> R {
    let pnp = fs::read_to_string("/proc/net/pnp").map_err(e("/proc/net/pnp"))?;
    fs::write("/etc/resolv.conf", &pnp).map_err(e("/etc/resolv.conf"))?;
    let t = TcpStream::connect_timeout(&"1.1.1.1:80".parse().unwrap(), Duration::from_secs(5));
    println!("INET tcp 1.1.1.1:80: {}", t.as_ref().map(|_| "connected".to_string()).unwrap_or_else(|x| x.to_string()));
    t.map_err(e("Internet TCP"))?;
    let host = "connectivitycheck.gstatic.com";
    let addr = (host, 80).to_socket_addrs().map_err(e("DNS"))?.find(SocketAddr::is_ipv4).ok_or("DNS: no IPv4 address")?;
    println!("DNS {host} -> {}", addr.ip());
    let (status, _) = http_get(addr, host, "/generate_204", "")?;
    println!("INET http://{host}/generate_204: {status}");
    if status.split_whitespace().nth(1) != Some("204") {
        return Err(format!("Internet HTTP: expected 204, got {status}"));
    }
    Ok(())
}

fn loopback(_: &mut State) -> R {
    let op = fs::read_to_string("/sys/class/net/lo/operstate").map_err(e("operstate"))?;
    let fl = fs::read_to_string("/sys/class/net/lo/flags").map_err(e("flags"))?;
    let flags = u32::from_str_radix(fl.trim().trim_start_matches("0x"), 16).unwrap_or(0);
    println!("LO: operstate={} flags={} IFF_UP={}", op.trim(), fl.trim(), flags & 1 == 1);
    if flags & 1 == 0 { return Err("loopback is not up".into()); }
    Ok(())
}

fn measurements(st: &mut State) -> R {
    let names = [
        "mrtd:sha384", "rtmr0:sha384", "rtmr1:sha384", "rtmr2:sha384", "rtmr3:sha384",
        "mrowner", "mrconfigid", "mrownerconfig",
    ];
    let mut failed = vec![];
    for n in names {
        match fs::read(format!("{MEAS}/{n}")) {
            Ok(v) => {
                println!("MR {n} {}", hex(&v));
                st.mrs.push((n.to_string(), v));
            }
            Err(err) => failed.push(format!("{n}: {err}")),
        }
    }
    let zero = |n| st.mr(n).map(|v| v.iter().all(|&b| b == 0));
    println!("MR rtmr2 all-zero: {:?}, rtmr3 all-zero: {:?}", zero("rtmr2:sha384"), zero("rtmr3:sha384"));
    if zero("rtmr2:sha384") != Some(true) || zero("rtmr3:sha384") != Some(true) {
        failed.push("RTMR2/RTMR3 are not zero before the probe extend".into());
    }
    if failed.is_empty() { Ok(()) } else { Err(failed.join("; ")) }
}

fn ccel(st: &mut State) -> R {
    if let Ok(t) = fs::read("/sys/firmware/acpi/tables/CCEL") {
        println!("CCEL_TABLE: {}", hex(&t));
    }
    let mut log = fs::read("/sys/firmware/acpi/tables/data/CCEL").map_err(e("data/CCEL"))?;
    let raw = log.len();
    while log.last() == Some(&0xFF) {
        log.pop();
    }
    println!("CCEL_LEN: {} (area {raw} bytes, trailing 0xFF trimmed)", log.len());
    println!("CCEL_SHA384: {}", hex(&sha384(&log)));
    println!("CCEL_B64: {}", base64(&log));
    let (events, stop) = tcg::parse(&log);
    if let Some(why) = stop {
        return Err(format!("CCEL parse stopped early: {why}"));
    }
    let mut regs = [[0u8; 48]; 5];
    for (i, ev) in events.iter().enumerate() {
        let d = ev.sha384.map(|d| hex(&d)).unwrap_or_else(|| "-".into());
        println!("EV {i:3} MR{} {} sha384={d} len={} {}", ev.mr, tcg::type_name(ev.etype), ev.data.len(), tcg::describe(ev));
        if ev.etype != tcg::EV_NO_ACTION && (ev.mr as usize) < regs.len() {
            if let Some(d) = ev.sha384 {
                regs[ev.mr as usize] = sha::extend(&regs[ev.mr as usize], &d);
            }
        }
    }
    println!("CCEL events: {}", events.len());
    let mut ok = true;
    for r in 0..3 {
        let name = format!("rtmr{r}:sha384");
        let replay = regs[r + 1];
        let m = st.mr(&name).map(|v| v == replay);
        ok &= m == Some(true);
        println!("REPLAY rtmr{r} {} {}", hex(&replay), match m { Some(true) => "MATCH", Some(false) => "MISMATCH", None => "(no sysfs value)" });
    }
    if ok { Ok(()) } else { Err("CCEL replay does not match RTMR0..2".into()) }
}

fn quote(st: &mut State) -> R {
    fs::create_dir(TSM).map_err(e("mkdir"))?;
    let r = quote_inner(st);
    let rm = fs::remove_dir(TSM).map_err(e("rmdir"));
    r.and(rm)
}

fn quote_inner(st: &mut State) -> R {
    let read = |f: &str| fs::read(format!("{TSM}/{f}")).map_err(e(f));
    let gen = |v: Vec<u8>| String::from_utf8_lossy(&v).trim().parse::<u64>().unwrap_or(u64::MAX);
    let provider = read("provider")?;
    let provider = String::from_utf8_lossy(&provider);
    println!("QUOTE provider: {}", provider.trim());
    if provider.trim() != "tdx_guest" { return Err("unexpected quote provider".into()); }
    let g0 = gen(read("generation")?);
    let inblob = sha512(b"vcontainer-probe report_data");
    let mut f = fs::OpenOptions::new().write(true).open(format!("{TSM}/inblob")).map_err(e("open inblob"))?;
    let n = f.write(&inblob).map_err(e("write inblob"))?;
    drop(f);
    if n != 64 {
        return Err(format!("short inblob write: {n}"));
    }
    let t = std::time::Instant::now();
    let out = read("outblob")?;
    let g1 = gen(read("generation")?);
    let generation_ok = g0.checked_add(1) == Some(g1);
    println!("QUOTE generation {g0} -> {g1} ({}), outblob {} bytes in {:?}", if generation_ok { "OK" } else { "UNEXPECTED" }, out.len(), t.elapsed());
    if !generation_ok { return Err("quote generation did not advance once".into()); }
    let sig_len = out.get(0x278..0x27C).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or("outblob too short")?;
    let len = 0x27C + sig_len as usize;
    let q = out.get(..len).ok_or("signature length beyond outblob")?;
    let padding_ok = out[len..].iter().all(|&b| b == 0);
    println!("QUOTE length {len} (padding all zero: {padding_ok})");
    if !padding_ok { return Err("nonzero quote padding".into()); }
    let u16at = |o: usize| u16::from_le_bytes([q[o], q[o + 1]]);
    let body = &q[48..];
    println!(
        "QUOTE header version={} att_key_type={} tee_type={:#x}",
        u16at(0), u16at(2), u32::from_le_bytes(q[4..8].try_into().unwrap())
    );
    println!("QUOTE tee_tcb_svn={}", hex(&body[0..16]));
    println!("QUOTE td_attributes={} xfam={}", hex(&body[120..128]), hex(&body[128..136]));
    let fields = [
        ("mrtd:sha384", 136), ("mrconfigid", 184), ("mrowner", 232), ("mrownerconfig", 280),
        ("rtmr0:sha384", 328), ("rtmr1:sha384", 376), ("rtmr2:sha384", 424), ("rtmr3:sha384", 472),
    ];
    for (name, off) in fields {
        let v = &body[off..off + 48];
        let same = st.mr(name).map(|m| m == v);
        println!("QUOTE {name} {} sysfs:{}", hex(v), match same { Some(true) => "same", Some(false) => "DIFFERENT", None => "n/a" });
        if same != Some(true) { return Err(format!("quote {name} does not match sysfs")); }
    }
    let report_data_ok = body[520..584] == inblob;
    println!("QUOTE report_data matches inblob: {report_data_ok}");
    if !report_data_ok { return Err("quote report_data mismatch".into()); }
    println!("QUOTE_B64: {}", base64(q));
    Ok(())
}

/// GCE sets MROWNER = SHA-384 of {"instanceId":N,"numericalProjectId":N,"zone":"Z"}.
fn pzid(st: &mut State) -> R {
    let (Some(id), Some(proj), Some(zone)) = (&st.instance_id, &st.project_id, &st.zone) else {
        return Err("metadata values missing".into());
    };
    let zone = zone.rsplit('/').next().unwrap_or(zone);
    let json = format!("{{\"instanceId\":{id},\"numericalProjectId\":{proj},\"zone\":\"{zone}\"}}");
    let h = sha384(json.as_bytes());
    let m = st.mr("mrowner").map(|v| v == h);
    println!("PZID {json}");
    println!("PZID sha384={} mrowner match: {:?}", hex(&h), m);
    if m != Some(true) { return Err("PZID does not match MROWNER".into()); }
    Ok(())
}

fn rtmr3(_: &mut State) -> R {
    let path = format!("{MEAS}/rtmr3:sha384");
    let before = fs::read(&path).map_err(e("read rtmr3"))?;
    let digest = sha384(PROBE_STRING);
    let mut f = fs::OpenOptions::new().write(true).open(&path).map_err(e("open rtmr3"))?;
    let n = f.write(&digest).map_err(e("write rtmr3"))?;
    drop(f);
    let after = fs::read(&path).map_err(e("re-read rtmr3"))?;
    let expected = sha::extend(&[0u8; 48], &digest);
    println!("RTMR3 before={} (zero: {})", hex(&before), before.iter().all(|&b| b == 0));
    println!("RTMR3 extend digest=sha384(\"vcontainer-probe\")={} wrote {n} bytes", hex(&digest));
    println!("RTMR3 after={}", hex(&after));
    println!("RTMR3 expected sha384(0^48||digest)={} {}", hex(&expected), if after == expected { "MATCH" } else { "MISMATCH" });
    if after == expected { Ok(()) } else { Err("RTMR3 extend result mismatch".into()) }
}

fn kmsg_tail(st: &mut State) -> R {
    let log = sys::kmsg_read_all().map_err(e("klogctl"))?;
    for l in log.get(st.kmsg_len..).unwrap_or("").lines() {
        println!("KMSG+ {l}");
    }
    Ok(())
}

#[cfg(test)]
mod result_tests {
    use super::*;

    #[test]
    fn failed_or_panicking_checks_are_counted() {
        let mut state = State::default();
        step(&mut state, "success", |_| Ok(()));
        assert_eq!(state.failures, 0);
        step(&mut state, "failure", |_| Err("check failed".into()));
        step(&mut state, "panic", |_| panic!("check panicked"));
        assert_eq!(state.failures, 2);
    }
}
