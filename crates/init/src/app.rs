//! Session state and the three API operations. One mutex orders ledger
//! extensions, state changes and quotes, so every quote matches the status
//! and ledger it is served with.

use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use vcontainer_core::crypto::{self, HpkeSecret};
use vcontainer_core::*;

use crate::sys::check;
use crate::tdx;

struct Launch {
    /// The launch event payload, which the owner's commit signs.
    payload: Vec<u8>,
    config: Config,
    secrets: Secrets,
}

/// Where the boot's single launch stands; `status` only reports it.
enum Step {
    /// The HPKE key opens one launch and is dropped when it is measured.
    Waiting(HpkeSecret),
    Measured(Box<Launch>),
    Committed,
}

struct Inner {
    status: Status,
    ledger: Vec<Event>,
    step: Step,
}

pub struct App {
    boot: BootInfo,
    owner: crypto::VerifyingKey,
    ccel: Vec<u8>,
    inner: Mutex<Inner>,
}

/// An API error with its HTTP status code.
pub struct ApiError(pub u16, pub String);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(400, format!("{:#}", e.into()))
    }
}

fn conflict(msg: &str) -> ApiError {
    ApiError(409, msg.to_string())
}

impl App {
    pub fn new(boot: BootInfo, owner: crypto::VerifyingKey, hpke_sk: HpkeSecret, ccel: Vec<u8>, ledger: Vec<Event>) -> Self {
        let status = Status { state: State::Waiting, exit_code: None, error: None };
        App { boot, owner, ccel, inner: Mutex::new(Inner { status, ledger, step: Step::Waiting(hpke_sk) }) }
    }

    pub fn evidence(&self, nonce: &[u8; 32]) -> Result<Evidence> {
        let g = self.inner.lock().unwrap();
        let status = g.status.to_bytes();
        let quote = tdx::quote(&report_data(nonce, &status))?;
        Ok(Evidence { v: 1, tee: "tdx".into(), status, ledger: g.ledger.clone(), quote, ccel: self.ccel.clone() })
    }

    /// Extends RTMR3 and the ledger under the lock that quotes take.
    pub fn measure(&self, event: Event) -> Result<()> {
        tdx::measure(&mut self.inner.lock().unwrap().ledger, event)
    }

    /// Measures a signed launch and waits for the owner's commit.
    pub fn launch(&self, req: LaunchRequest) -> Result<(), ApiError> {
        let payload = launch_payload(req.issued_at, &req.sealed, &req.config);
        crypto::verify(&self.owner, &owner_message("launch", &self.boot, &payload), &req.sig).map_err(|e| ApiError(403, format!("{e:#}")))?;
        let config = Config::parse(&req.config)?;

        // Only owner-signed requests get here; nothing changes until every
        // check below passes.
        let mut g = self.inner.lock().unwrap();
        let Step::Waiting(sk) = &g.step else { return Err(conflict("this boot has already been launched")) };
        let secrets: Secrets = serde_json::from_slice(&crypto::open(sk, &self.boot.boot_id, &req.sealed)?)?;
        if !secrets.files.keys().eq(&config.secrets) {
            return Err(ApiError(400, "sealed secrets do not match the declared names".into()));
        }
        if config.registry_token != secrets.registry_token.is_some() {
            return Err(ApiError(400, "registry_token must be true exactly when a token is sealed".into()));
        }
        // The owner's signed time is a floor for the guest clock, so the host
        // cannot roll time back into an expired certificate's validity.
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if now < req.issued_at {
            let ts = libc::timespec { tv_sec: req.issued_at as _, tv_nsec: 0 };
            check(unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) }, "clock_settime").map_err(|e| ApiError(500, format!("{e:#}")))?;
        }
        tdx::measure(&mut g.ledger, Event::new(EV_LAUNCH, payload.clone())).map_err(|e| ApiError(500, format!("{e:#}")))?;
        g.status.state = State::Starting;
        g.step = Step::Measured(Box::new(Launch { payload, config, secrets }));
        crate::sys::log("launch measured; waiting for commit");
        Ok(())
    }

    /// The owner saw the measured launch; start pulling.
    pub fn commit(self: &Arc<Self>, req: CommitRequest) -> Result<(), ApiError> {
        let mut g = self.inner.lock().unwrap();
        let Step::Measured(l) = &g.step else { return Err(conflict("nothing to commit")) };
        crypto::verify(&self.owner, &owner_message("commit", &self.boot, &l.payload), &req.sig).map_err(|e| ApiError(403, format!("{e:#}")))?;
        let Step::Measured(l) = std::mem::replace(&mut g.step, Step::Committed) else { unreachable!() };
        g.status.state = State::Pulling;
        let app = self.clone();
        std::thread::spawn(move || crate::deploy::run(app, l.config, l.secrets));
        Ok(())
    }

    pub fn set_state(&self, state: State, exit_code: Option<i32>, error: Option<String>) {
        if let Some(e) = &error {
            crate::sys::log(&format!("container: {e}"));
        }
        let mut g = self.inner.lock().unwrap();
        g.status = Status { state, exit_code, error };
    }
}
