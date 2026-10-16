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
use std::sync::Arc;
use vcontainer_core::{crypto, hex, BootInfo, Event, EV_BOOT, EV_OWNER, OWNER_METADATA_KEY};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--stage") {
        let result = match (args.get(2).map(String::as_str), args.get(3)) {
            (Some("unpack"), Some(root)) => unpack::main(root),
            (Some("run"), _) => run::main(),
            _ => Err(anyhow::anyhow!("unknown stage")),
        };
        if let Err(e) = result {
            sys::log(&format!("{e:#}"));
            std::process::exit(1);
        }
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
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    let null = unsafe { libc::open(sys::cstr("/dev/null").as_ptr(), libc::O_RDONLY) };
    unsafe { libc::dup2(null, 0) };
    tdx::check()?;

    let (hpke_sk, hpke_pk) = crypto::hpke_keypair();
    let boot = BootInfo { boot_id: sys::random::<16>(), hpke_pk };
    let boot_event = Event::new(EV_BOOT, boot.encode());
    tdx::extend_rtmr3(&boot_event.digest())?;
    let mut ledger = vec![boot_event];
    let ccel = tdx::ccel();

    net::setup().context("network")?;
    let owner_raw = net::metadata(&format!("instance/attributes/{OWNER_METADATA_KEY}"))?;
    let owner_event = Event::new(EV_OWNER, owner_raw.clone());
    tdx::extend_rtmr3(&owner_event.digest())?;
    ledger.push(owner_event);
    let owner = crypto::parse_owner(&owner_raw).context("owner key in metadata")?;

    sys::log(&format!("waiting for launch on :{} (boot {})", vcontainer_core::PORT, hex(&boot.boot_id)));
    Ok(Arc::new(app::App::new(boot, owner, hpke_sk, ccel, ledger)))
}
