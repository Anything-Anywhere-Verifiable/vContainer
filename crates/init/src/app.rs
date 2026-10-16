//! Session state and the three API operations. One mutex orders ledger
//! extensions, state changes and quotes, so every quote matches the status
//! and ledger it is served with.

use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use vcontainer_core::crypto::{self, HpkeSecret};
use vcontainer_core::*;

use crate::tdx;

pub struct Launch {
    pub config_bytes: Vec<u8>,
    pub config: Config,
    pub secrets: Secrets,
}

pub struct Inner {
    pub status: Status,
    pub ledger: Vec<Event>,
    pub launch: Option<Launch>,
}

pub struct App {
    pub boot: BootInfo,
    owner: crypto::VerifyingKey,
    hpke_sk: HpkeSecret,
    ccel: Vec<u8>,
    pub inner: Mutex<Inner>,
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

pub fn extend(inner: &mut Inner, event: Event) -> Result<()> {
    tdx::extend_rtmr3(&event.digest())?;
    inner.ledger.push(event);
    Ok(())
}

impl App {
    pub fn new(boot: BootInfo, owner: crypto::VerifyingKey, hpke_sk: HpkeSecret, ccel: Vec<u8>, ledger: Vec<Event>) -> Self {
        let status = Status { state: State::Waiting, exit_code: None, error: None };
        App { boot, owner, hpke_sk, ccel, inner: Mutex::new(Inner { status, ledger, launch: None }) }
    }

    pub fn evidence(&self, nonce: &[u8; 32]) -> Result<Evidence> {
        let g = self.inner.lock().unwrap();
        let status = g.status.to_bytes();
        let quote = tdx::quote(&report_data(nonce, &status))?;
        Ok(Evidence { v: 1, tee: "tdx".into(), status, ledger: g.ledger.clone(), quote, ccel: self.ccel.clone() })
    }

    fn check_boot_id(&self, boot_id: &str) -> Result<(), ApiError> {
        if unhex_array::<16>(boot_id)? != self.boot.boot_id {
            return Err(ApiError(403, "launch is for another boot".into()));
        }
        Ok(())
    }

    /// Measures a signed launch and waits for the owner's commit.
    pub fn launch(&self, req: LaunchRequest) -> Result<(), ApiError> {
        self.check_boot_id(&req.boot_id)?;
        // Side-effect-free checks first, so junk requests cannot take the slot.
        let msg = launch_message(&self.boot, req.issued_at, &req.config, req.sealed.as_ref());
        crypto::verify(&self.owner, &msg, &req.sig).map_err(|e| ApiError(403, format!("{e:#}")))?;
        let config = Config::parse(&req.config)?;
        let secrets: Secrets = match &req.sealed {
            Some(s) => serde_json::from_slice(&crypto::open(&self.hpke_sk, &self.boot.boot_id, s)?)?,
            None => Secrets::default(),
        };
        for name in &config.secrets {
            if !secrets.files.contains_key(name) {
                return Err(ApiError(400, format!("secret {name} is declared but not sealed")));
            }
        }
        if config.registry_token && secrets.registry_token.is_none() {
            return Err(ApiError(400, "registry_token is set but no token is sealed".into()));
        }

        let mut g = self.inner.lock().unwrap();
        if g.status.state != State::Waiting {
            return Err(conflict("this boot has already been launched"));
        }
        // The owner's signed time is a floor for the guest clock, so the host
        // cannot roll time back into an expired certificate's validity.
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if now < req.issued_at {
            let ts = libc::timespec { tv_sec: req.issued_at as _, tv_nsec: 0 };
            unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) };
        }
        extend(&mut g, Event::new(EV_LAUNCH, launch_payload(&req.config, req.sealed.as_ref())))
            .map_err(|e| ApiError(500, format!("{e:#}")))?;
        g.status.state = State::Starting;
        g.launch = Some(Launch { config_bytes: req.config, config, secrets });
        crate::sys::log("launch measured; waiting for commit");
        Ok(())
    }

    /// The owner saw the measured launch; start pulling.
    pub fn commit(self: &Arc<Self>, req: CommitRequest) -> Result<(), ApiError> {
        self.check_boot_id(&req.boot_id)?;
        let mut g = self.inner.lock().unwrap();
        if g.status.state != State::Starting {
            return Err(conflict("nothing to commit"));
        }
        let launch = g.launch.as_ref().context("launch missing")?;
        let msg = commit_message(&self.boot.boot_id, &launch.config_bytes);
        crypto::verify(&self.owner, &msg, &req.sig).map_err(|e| ApiError(403, format!("{e:#}")))?;
        g.status.state = State::Pulling;
        drop(g);
        let app = self.clone();
        std::thread::spawn(move || crate::deploy::run(app));
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
