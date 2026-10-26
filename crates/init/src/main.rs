//! vcontainer-init: PID 1 of a vcontainer confidential VM.
//!
//! Boot: mount the kernel filesystems, measure a fresh boot id and HPKE key
//! (ledger event 0), bring up routes, read and measure the owner's public key
//! from instance metadata (event 1), then serve the API on :7700 and wait for
//! a signed launch (event 2) and the owner's commit. Only then is the image
//! pulled; the ledger is sealed (event 3) before the container starts.

mod app;
mod deploy;
mod http;
mod net;
mod run;
mod sys;
mod tdx;
mod unpack;

use anyhow::{Context, Result};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use vcontainer_core::{crypto, hex, BootInfo, Event, EV_BOOT, EV_OWNER, OWNER_METADATA_KEY};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--stage") {
        // Helpers report on stdout, since PID 1 reaps every exit status:
        // progress lines, then `ok` or `failed <reason>`.
        let result = match args.get(2).map(String::as_str) {
            Some("unpack") => unpack::main(),
            Some("run") => run::main(),
            _ => Err(anyhow::anyhow!("unknown stage")),
        };
        if let Err(e) = result {
            sys::log(&format!("{e:#}"));
            println!("failed {}", format!("{e:#}").replace('\n', " "));
            std::process::exit(1);
        }
        println!("ok");
        std::process::exit(0);
    }
    match boot() {
        Ok(app) => {
            std::thread::spawn(move || {
                if let Err(e) = http::serve(app) {
                    sys::log(&format!("FAIL api: {e:#}"));
                    sys::poweroff();
                }
            });
            sys::reap_forever();
        }
        Err(e) => {
            sys::log(&format!("FAIL boot: {e:#}"));
            sys::poweroff();
        }
    }
}

fn boot() -> Result<Arc<app::App>> {
    sys::mount("proc", "/proc", "proc", 0, None)?;
    sys::mount("sysfs", "/sys", "sysfs", 0, None)?;
    sys::mount("devtmpfs", "/dev", "devtmpfs", 0, None)?;
    sys::mount("tmpfs", "/run", "tmpfs", 0, Some("mode=0755"))?;
    sys::mount("configfs", "/sys/kernel/config", "configfs", 0, None)?;
    sys::check(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) }, "prctl")?;
    // stdin is /dev/null, never the host-controlled console. Only fd 0 is
    // inherited: dup2 clears CLOEXEC there.
    let null = std::fs::File::open("/dev/null")?;
    sys::check(unsafe { libc::dup2(null.as_raw_fd(), 0) }, "dup2")?;
    // Platform checks read only the log before ipconfig's record: the DHCP
    // host and domain names it prints are the first host-supplied free text.
    let log = sys::kernel_log()?;
    let (kernel, dhcp) = log.split_once("IP-Config: Complete:").context("kernel IP autoconfiguration did not complete")?;
    tdx::check(kernel)?;

    let (hpke_sk, hpke_pk) = crypto::hpke_keypair();
    let boot = BootInfo { boot_id: crypto::random::<16>(), hpke_pk };
    let mut ledger = Vec::new();
    tdx::measure(&mut ledger, Event::new(EV_BOOT, boot.encode()))?;
    let ccel = tdx::ccel();

    net::setup(dhcp).context("network")?;
    let owner_raw = net::metadata(&format!("instance/attributes/{OWNER_METADATA_KEY}"))?;
    tdx::measure(&mut ledger, Event::new(EV_OWNER, owner_raw.clone()))?;
    let owner = crypto::parse_owner(&owner_raw).context("owner key in metadata")?;

    sys::log(&format!("waiting for launch on :{} (boot {})", vcontainer_core::PORT, hex(&boot.boot_id)));
    Ok(Arc::new(app::App::new(boot, owner, hpke_sk, ccel, ledger)))
}
