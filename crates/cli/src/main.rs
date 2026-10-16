//! vcontainer: run containers in verified Intel TDX confidential VMs on Google Cloud.
//!
//!   vcontainer run --session NAME            start a TDX VM and verify it (nothing secret sent yet)
//!   vcontainer deploy --session NAME IMAGE   verify again, then send the container (alias: pull)
//!   vcontainer verify --session NAME         verify at any time; third parties use --url/--policy

mod ccel;
mod endorse;
mod gcp;
mod measure;
mod state;
mod verify;

use anyhow::{bail, ensure, Context, Result};
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vcontainer_core::registry::{ImageRef, Registry};
use vcontainer_core::*;

use state::{Policy, Session};

/// Oldest acceptable Intel TCB evaluation data number.
const MIN_TCB_EVAL_NUM: u32 = 20;

#[derive(Parser)]
#[command(name = "vcontainer", version, about = "Run containers in verified Intel TDX confidential VMs on Google Cloud")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a session (a TDX confidential VM), or reconnect to it, and verify it.
    Run {
        #[arg(long)]
        session: String,
        #[arg(long, default_value = "us-central1-a")]
        zone: String,
        #[arg(long, default_value = "c3-standard-4")]
        machine_type: String,
        /// Address range allowed to reach the session (default: this machine's public IP).
        #[arg(long)]
        allow_from: Option<String>,
    },
    /// Verify a session, then deploy a container into it (alias: pull).
    #[command(alias = "pull")]
    Deploy {
        #[arg(long)]
        session: String,
        /// Image reference; a tag is resolved to a digest before launch.
        image: String,
        /// Environment variable KEY=VALUE (measured and public: not for secrets).
        #[arg(short, long = "env")]
        env: Vec<String>,
        /// Secret NAME=@FILE, NAME=VALUE or NAME (from your environment), sealed
        /// to the verified VM and mounted at /run/secrets/NAME.
        #[arg(long = "secret")]
        secrets: Vec<String>,
        /// Container port to open to --allow-from.
        #[arg(short, long = "port")]
        ports: Vec<u16>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        workdir: Option<String>,
        #[arg(long)]
        entrypoint: Option<String>,
        /// Do not copy the container's output to the serial console.
        #[arg(long)]
        quiet: bool,
        /// Command and arguments (after --).
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Verify a session, or any vcontainer endpoint against a policy file.
    Verify {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        policy: Option<PathBuf>,
        /// Write the session's policy (what a third party needs) to this file.
        #[arg(long)]
        export_policy: Option<PathBuf>,
    },
    /// List sessions.
    Ls,
    /// Print a session's serial console (kernel, /init and container output).
    Logs {
        #[arg(long)]
        session: String,
    },
    /// Delete a session's VM and firewall rule.
    Rm {
        #[arg(long)]
        session: String,
    },
    /// Print the expected measurements of a built boot image.
    Measure {
        #[arg(long, default_value = "out")]
        dir: PathBuf,
    },
    /// Upload a built boot image to Compute Engine (one-time setup).
    Publish {
        #[arg(long, default_value = "out")]
        dir: PathBuf,
        /// Cloud Storage bucket for the upload (default: PROJECT-vcontainer).
        #[arg(long)]
        bucket: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Run { session, zone, machine_type, allow_from } => run(&session, &zone, &machine_type, allow_from),
        Cmd::Deploy { session, image, env, secrets, ports, user, workdir, entrypoint, quiet, cmd } => {
            let config = Config {
                image: String::new(),
                entrypoint: entrypoint.map(|e| vec![e]),
                cmd: (!cmd.is_empty()).then_some(cmd),
                env,
                workdir,
                user,
                secrets: Vec::new(),
                registry_token: false,
                logs: if quiet { Logs::None } else { Logs::Serial },
            };
            deploy(&session, &image, config, &secrets, &ports)
        }
        Cmd::Verify { session, url, policy, export_policy } => verify_cmd(session, url, policy, export_policy),
        Cmd::Ls => ls(),
        Cmd::Logs { session } => logs(&session),
        Cmd::Rm { session } => rm(&session),
        Cmd::Measure { dir } => measure::measure(&dir.join("BOOTX64.EFI"), &dir.join("disk.raw")).map(|m| measure::print(&m)),
        Cmd::Publish { dir, bucket } => publish(&dir, bucket),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(name: &str, zone: &str, machine_type: &str, allow_from: Option<String>) -> Result<()> {
    state::check_name(name)?;
    let owner = state::owner_key()?;
    let release = state::release()?;
    let previous = state::load_session(name).ok();
    let zone = previous.as_ref().map_or(zone, |p| p.zone.as_str()).to_string();
    let project = gcp::project()?;
    let source = match (allow_from, &previous) {
        (Some(c), _) => c,
        (None, Some(p)) => p.source.clone(),
        (None, None) => format!("{}/32", gcp::my_ip()?),
    };
    let inst = match gcp::instance(name, &zone)? {
        Some(i) if i.status == "RUNNING" => i,
        Some(i) => {
            eprintln!("starting {name} (was {})", i.status);
            gcp::start(name, &zone)?;
            gcp::instance(name, &zone)?.context("instance disappeared")?
        }
        None => {
            gcp::ensure_network()?;
            gcp::ensure_firewall(name, &[], &source)?;
            eprintln!("creating TDX confidential VM {name} ({machine_type} in {zone})");
            gcp::create_instance(name, &zone, machine_type, &release.image, &release.project, &crypto::owner_metadata(&owner))?;
            gcp::instance(name, &zone)?.context("instance disappeared")?
        }
    };
    let instance = state::Instance { id: inst.id.parse()?, project_number: gcp::project_number(&project)?, zone: zone.clone() };
    let policy = Policy {
        tee: "tdx".into(),
        owner: crypto::owner_metadata(&owner),
        release,
        instance: Some(instance),
        config: previous.as_ref().and_then(|p| p.policy.config.clone()),
        min_tcb_eval_num: MIN_TCB_EVAL_NUM,
    };
    let mut s = Session {
        name: name.into(),
        project,
        zone,
        machine_type: machine_type.into(),
        ip: inst.ip().context("instance has no external IP")?,
        source,
        ports: previous.map(|p| p.ports).unwrap_or_default(),
        policy,
    };
    eprintln!("waiting for {} to boot and answer attestation", s.url());
    let v = wait_verified(&s, Duration::from_secs(300))?;
    if v.status.state == State::Waiting {
        s.policy.config = None; // a fresh boot: whatever ran before is gone
    }
    state::save_session(&s)?;
    report(&s, &v);
    if v.status.state == State::Waiting {
        println!("\nsession {name} is ready. Deploy a container:\n  vcontainer deploy --session {name} IMAGE [-p PORT] [--secret NAME=@FILE]");
    }
    Ok(())
}

fn wait_verified(s: &Session, timeout: Duration) -> Result<verify::Verified> {
    let start = Instant::now();
    for i in 0.. {
        let nonce = verify::nonce();
        let last = match verify::fetch_evidence(&s.url(), &nonce) {
            Ok(ev) => return verify::verify(&ev, &nonce, &s.policy),
            Err(e) => format!("{e:#}"),
        };
        if i % 6 == 5 {
            if let Some(inst) = gcp::instance(&s.name, &s.zone)? {
                if !["RUNNING", "PROVISIONING", "STAGING"].contains(&inst.status.as_str()) {
                    bail!("the VM is {}; its console says:\n{}", inst.status, serial_tail(s));
                }
            }
        }
        if start.elapsed() > timeout {
            bail!("no answer from {} after {timeout:?} ({last}); console:\n{}", s.url(), serial_tail(s));
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    unreachable!()
}

fn serial_tail(s: &Session) -> String {
    let out = gcp::serial(&s.name, &s.zone).unwrap_or_default();
    let lines: Vec<&str> = out.lines().collect();
    lines[lines.len().saturating_sub(25)..].join("\n")
}

fn report(s: &Session, v: &verify::Verified) {
    println!("session {} ({}, {}): VERIFIED", s.name, s.ip, s.zone);
    for c in &v.claims {
        println!("  ✓ {c}");
    }
    let st = &v.status;
    print!("  state: {:?}", st.state);
    if let Some(code) = st.exit_code {
        print!(" (exit code {code})");
    }
    if let Some(e) = &st.error {
        print!(" — {e}");
    }
    println!();
}

/// `NAME=@FILE`, `NAME=VALUE` or `NAME` (read from the environment).
fn parse_secret(spec: &str) -> Result<(String, Vec<u8>)> {
    let (name, value) = match spec.split_once('=') {
        Some((n, v)) => match v.strip_prefix('@') {
            Some(path) => (n, std::fs::read(path).with_context(|| format!("secret {n}: read {path}"))?),
            None => (n, v.as_bytes().to_vec()),
        },
        None => (spec, std::env::var(spec).with_context(|| format!("secret {spec}: not set in the environment"))?.into_bytes()),
    };
    ensure!(valid_name(name), "invalid secret name {name:?}");
    Ok((name.to_string(), value))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn deploy(name: &str, image: &str, mut config: Config, secret_specs: &[String], ports: &[u16]) -> Result<()> {
    let owner = state::owner_key()?;
    let mut s = state::load_session(name)?;
    let nonce = verify::nonce();
    let v = verify::verify(&verify::fetch_evidence(&s.url(), &nonce)?, &nonce, &s.policy)?;
    ensure!(
        v.status.state == State::Waiting,
        "session {name} already ran a container (state {:?}); use a new session, or `vcontainer rm --session {name}` and run it again",
        v.status.state
    );
    eprintln!("session verified: {}", v.claims.join("; "));

    let r = ImageRef::parse(image)?;
    let token = if r.is_google() { Some(gcp::access_token()?) } else { None };
    let digest = Registry::new(&r, token.clone().map(|t| ("oauth2accesstoken".into(), t)))
        .resolve()
        .with_context(|| format!("resolving {image}"))?;
    config.image = r.pinned(&digest);
    config.registry_token = token.is_some();
    let mut files = BTreeMap::new();
    for spec in secret_specs {
        let (n, value) = parse_secret(spec)?;
        config.secrets.push(n.clone());
        files.insert(n, b64::encode(&value));
    }
    let config_bytes = serde_json::to_vec(&config)?;
    let sealed = if files.is_empty() && token.is_none() {
        None
    } else {
        Some(crypto::seal(&v.boot, &serde_json::to_vec(&Secrets { files, registry_token: token })?)?)
    };
    let issued_at = now();
    let sig = crypto::sign(&owner, &launch_message(&v.boot, issued_at, &config_bytes, sealed.as_ref()));
    let boot_id = hex(&v.boot.boot_id);
    eprintln!("launching {}", config.image);
    verify::post(&s.url(), "/v1/launch", &LaunchRequest { boot_id: boot_id.clone(), issued_at, config: config_bytes.clone(), sealed, sig })?;

    // Commit only after seeing exactly this launch measured, before any pull.
    s.policy.config = Some(b64::encode(&config_bytes));
    let n = verify::nonce();
    let v2 = verify::verify(&verify::fetch_evidence(&s.url(), &n)?, &n, &s.policy)?;
    ensure!(v2.status.state == State::Starting && v2.boot == v.boot, "unexpected state after launch: {:?}", v2.status.state);
    let commit = CommitRequest { boot_id, sig: crypto::sign(&owner, &commit_message(&v.boot.boot_id, &config_bytes)) };
    verify::post(&s.url(), "/v1/commit", &commit)?;
    eprintln!("launch measured and committed; the VM is pulling the image");
    if !ports.is_empty() {
        s.ports = ports.to_vec();
        gcp::ensure_firewall(name, &s.ports, &s.source)?;
    }
    state::save_session(&s)?;

    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(3));
        let n = verify::nonce();
        let ev = verify::fetch_evidence(&s.url(), &n)?;
        // Poll on the (unverified) status; verify fully once it settles.
        let st: Status = serde_json::from_slice(&ev.status)?;
        if matches!(st.state, State::Running | State::Exited | State::Failed) {
            let v = verify::verify(&ev, &n, &s.policy)?;
            report(&s, &v);
            if v.status.state == State::Failed {
                bail!("the container did not start; see `vcontainer logs --session {name}`");
            }
            for p in &s.ports {
                println!("  port {p}: http://{}:{p}", s.ip);
            }
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(1800) {
            bail!("still {:?} after 30 minutes", st.state);
        }
    }
}

fn verify_cmd(session: Option<String>, url: Option<String>, policy: Option<PathBuf>, export: Option<PathBuf>) -> Result<()> {
    let (url, policy, label) = match (session, url, policy) {
        (Some(name), None, None) => {
            let s = state::load_session(&name)?;
            (s.url(), s.policy, name)
        }
        (None, Some(url), Some(p)) => (url.clone(), serde_json::from_slice(&std::fs::read(&p)?)?, url),
        _ => bail!("use --session NAME, or --url URL --policy FILE"),
    };
    let n = verify::nonce();
    let v = verify::verify(&verify::fetch_evidence(&url, &n)?, &n, &policy)?;
    println!("{label}: VERIFIED");
    for c in &v.claims {
        println!("  ✓ {c}");
    }
    println!("  state: {:?}", v.status.state);
    if let Some(c) = &v.config {
        println!("  container: {} env={:?} secrets={:?}", c.image, c.env, c.secrets);
    }
    if let Some(path) = export {
        std::fs::write(&path, serde_json::to_vec_pretty(&policy)?)?;
        println!("policy written to {} (give it to anyone who should verify this session)", path.display());
    }
    Ok(())
}

fn ls() -> Result<()> {
    for s in state::sessions()? {
        let image = s.policy.config.as_ref().and_then(|c| Config::parse(&b64::decode(c).ok()?).ok()).map(|c| c.image).unwrap_or_else(|| "-".into());
        println!("{:<20} {:<16} {:<15} {}", s.name, s.zone, s.ip, image);
    }
    Ok(())
}

fn logs(name: &str) -> Result<()> {
    let s = state::load_session(name)?;
    println!("{}", gcp::serial(&s.name, &s.zone)?);
    Ok(())
}

fn rm(name: &str) -> Result<()> {
    let s = state::load_session(name)?;
    gcp::delete(&s.name, &s.zone)?;
    state::remove_session(name)?;
    println!("deleted session {name}");
    Ok(())
}

fn publish(dir: &std::path::Path, bucket: Option<String>) -> Result<()> {
    let project = gcp::project()?;
    let m = measure::measure(&dir.join("BOOTX64.EFI"), &dir.join("disk.raw"))?;
    measure::print(&m);
    let image = format!("vcontainer-{}", &hex(&m.pe_sha384)[..12]);
    let bucket = bucket.unwrap_or_else(|| format!("{project}-vcontainer"));
    if gcp::gcloud(&["storage", "buckets", "describe", &format!("gs://{bucket}"), "--format=value(name)"]).is_err() {
        gcp::gcloud(&["storage", "buckets", "create", &format!("gs://{bucket}"), "--location=us-central1", "--uniform-bucket-level-access"])?;
    }
    if gcp::gcloud(&["compute", "images", "describe", &image, "--format=value(name)"]).is_err() {
        let object = format!("gs://{bucket}/{image}.tar.gz");
        eprintln!("uploading {object}");
        gcp::gcloud(&["storage", "cp", dir.join("vcontainer.tar.gz").to_str().unwrap(), &object])?;
        eprintln!("creating image {image}");
        gcp::gcloud(&[
            "compute", "images", "create", &image,
            &format!("--source-uri={object}"),
            "--guest-os-features=UEFI_COMPATIBLE,GVNIC,TDX_CAPABLE",
            "--storage-location=us-central1",
            "--labels=vcontainer=image",
        ])?;
    }
    state::save_release(&state::Release {
        image: image.clone(),
        project,
        pe_authenticode_sha384: hex(&m.pe_sha384),
        gpt_event_sha384: hex(&m.gpt_sha384),
        rtmr1: m.rtmr1.iter().map(|r| hex(r)).collect(),
    })?;
    println!("published {image}; new sessions boot it");
    Ok(())
}
