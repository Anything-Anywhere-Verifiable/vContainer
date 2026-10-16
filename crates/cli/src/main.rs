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
/// Lowest acceptable security version of Google's TDX firmware (as of October 2026).
const MIN_FW_SVN: u64 = 2;

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
        /// Start the launch an interrupted deploy already sent, instead of a new one.
        #[arg(long, conflicts_with = "LaunchArgs")]
        resume: bool,
        #[command(flatten)]
        launch: LaunchArgs,
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

/// A new launch for `deploy`.
#[derive(clap::Args)]
struct LaunchArgs {
    /// Image reference; a tag is resolved to a digest before launch.
    #[arg(required_unless_present = "resume")]
    image: Option<String>,
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
    /// Do not copy the container's output to the serial console (by default
    /// it goes there, where the host and project members can read it).
    #[arg(long)]
    quiet: bool,
    /// Command and arguments (after --).
    #[arg(last = true)]
    cmd: Vec<String>,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Run { session, zone, machine_type, allow_from } => run(&session, &zone, &machine_type, allow_from),
        Cmd::Deploy { session, resume, launch } => deploy(&session, (!resume).then_some(launch)),
        Cmd::Verify { session, url, policy, export_policy } => verify_cmd(session, url, policy, export_policy),
        Cmd::Ls => ls(),
        Cmd::Logs { session } => logs(&session),
        Cmd::Rm { session } => rm(&session),
        Cmd::Measure { dir } => measure::measure(&dir).map(|m| measure::print(&m)),
        Cmd::Publish { dir, bucket } => publish(&dir, bucket),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(name: &str, zone: &str, machine_type: &str, allow_from: Option<String>) -> Result<()> {
    let prev = state::find_session(name)?;
    let owner = crypto::owner_metadata(&state::owner_key()?);
    let source = match (allow_from, &prev) {
        (Some(c), _) => c,
        (None, Some(p)) => p.source.clone(),
        (None, None) => format!("{}/32", gcp::my_ip()?),
    };
    // A session keeps where and what it booted.
    let (project, zone, release, launch, ports) = match prev {
        Some(p) => (p.project, p.zone, p.policy.release, p.policy.launch, p.ports),
        None => (gcp::project()?, zone.to_string(), state::release()?, None, vec![]),
    };
    gcp::ensure_network(&project)?;
    gcp::ensure_firewall(&project, name, &ports, &source)?;
    let inst = match gcp::instance(&project, name, &zone)? {
        Some(i) if i.status == "RUNNING" => i,
        Some(i) => {
            eprintln!("starting {name} (was {})", i.status);
            gcp::start(&project, name, &zone)?;
            gcp::instance(&project, name, &zone)?.context("instance disappeared")?
        }
        None => {
            eprintln!("creating TDX confidential VM {name} ({machine_type} in {zone})");
            gcp::create_instance(&project, name, &zone, machine_type, &release.image, &release.project, &owner)?;
            gcp::instance(&project, name, &zone)?.context("instance disappeared")?
        }
    };
    let instance = state::Instance { id: inst.id.parse()?, project_number: gcp::project_number(&project)?, zone: zone.clone() };
    let policy = Policy { tee: "tdx".into(), owner, release, instance, launch, min_tcb_eval_num: MIN_TCB_EVAL_NUM, min_fw_svn: MIN_FW_SVN };
    let mut s = Session { name: name.into(), project, zone, ip: inst.ip().context("instance has no external IP")?, source, ports, policy };
    state::save_session(&s)?; // so `rm` works even if verification fails
    eprintln!("waiting for {} to boot and answer attestation", s.url());
    let v = wait_verified(&s, Duration::from_secs(300))?;
    if v.status.state == State::Waiting {
        // A fresh boot: whatever was launched before is gone.
        s.policy.launch = None;
        s.ports.clear();
        state::save_session(&s)?;
    }
    report(&format!("session {name} ({}, {})", s.ip, s.zone), &v);
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
            if let Some(inst) = gcp::instance(&s.project, &s.name, &s.zone)? {
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

/// The last lines of the serial console, which the host can write to.
fn serial_tail(s: &Session) -> String {
    let out = gcp::serial(&s.project, &s.name, &s.zone).unwrap_or_default();
    let lines: Vec<&str> = out.lines().collect();
    lines[lines.len().saturating_sub(25)..].iter().map(|l| verify::untrusted(l.as_bytes())).collect::<Vec<_>>().join("\n")
}

fn report(label: &str, v: &verify::Verified) {
    println!("{label}: VERIFIED");
    for c in &v.claims {
        println!("  ✓ {c}");
    }
    let st = &v.status;
    print!("  state: {:?}", st.state);
    if let Some(code) = st.exit_code {
        print!(" (exit code {code})");
    }
    if let Some(e) = &st.error {
        print!(" — {}", verify::untrusted(e.as_bytes()));
    }
    println!();
    if let Some(c) = &v.config {
        println!("  container: {} env={:?} secrets={:?}", c.image, c.env, c.secrets);
    }
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

/// Sends `args` as a new launch, or with None resumes the saved one.
fn deploy(name: &str, args: Option<LaunchArgs>) -> Result<()> {
    let mut s = state::load_session(name)?;
    let owner = state::owner_key()?;
    let nonce = verify::nonce();
    let v = verify::verify(&verify::fetch_evidence(&s.url(), &nonce)?, &nonce, &s.policy)?;
    let image = v.config.as_ref().map_or("?", |c| c.image.as_str());
    let payload = match (v.status.state, args) {
        (State::Waiting, Some(args)) => {
            eprintln!("session verified: {}", v.claims.join("; "));
            launch(&mut s, &owner, &v.boot, args)?
        }
        (State::Starting, None) => {
            let saved = s.policy.launch.as_deref().context("no saved launch to resume")?;
            eprintln!("resuming the launch sent earlier: {image}");
            b64::decode(saved)?
        }
        (State::Starting, Some(_)) => bail!(
            "an earlier deploy to {name} was interrupted after its launch ({image}); start it with `vcontainer deploy --session {name} --resume`, or discard it with `vcontainer rm --session {name}`"
        ),
        (st, None) => bail!("session {name} has no interrupted launch to resume (state {st:?})"),
        (st, Some(_)) => bail!(
            "session {name} already ran a container (state {st:?}). For a fresh boot of the same VM: `gcloud compute instances reset {name} --zone {}` then `vcontainer run --session {name}`",
            s.zone
        ),
    };

    // Commit only after seeing this launch measured, before any pull: verify()
    // returns a config only if ledger event 2 is s.policy.launch, i.e. `payload`.
    let n = verify::nonce();
    let v2 = verify::verify(&verify::fetch_evidence(&s.url(), &n)?, &n, &s.policy)?;
    ensure!(
        v2.status.state == State::Starting && v2.boot == v.boot && v2.config.is_some(),
        "the session did not measure our launch (state {:?})",
        v2.status.state
    );
    gcp::ensure_firewall(&s.project, name, &s.ports, &s.source)?;
    let commit = CommitRequest { sig: crypto::sign(&owner, &owner_message("commit", &v.boot, &payload)) };
    verify::post(&s.url(), "/v1/commit", &commit)?;
    eprintln!("launch measured and committed; the VM is pulling the image");

    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(3));
        if start.elapsed() > Duration::from_secs(1800) {
            bail!("the container did not start within 30 minutes; see `vcontainer logs --session {name}`");
        }
        let n = verify::nonce();
        let Ok(ev) = verify::fetch_evidence(&s.url(), &n) else { continue };
        // Poll on the (unverified) status; verify fully once it settles.
        let st: Status = serde_json::from_slice(&ev.status)?;
        if st.state == State::Waiting {
            bail!("the VM restarted during the pull; run `vcontainer run --session {name}` and deploy again");
        }
        if matches!(st.state, State::Running | State::Exited | State::Failed) {
            let v = verify::verify(&ev, &n, &s.policy)?;
            report(&format!("session {name} ({}, {})", s.ip, s.zone), &v);
            match (v.status.state, v.status.exit_code) {
                (State::Failed, _) => bail!("the container did not start; see `vcontainer logs --session {name}`"),
                (State::Exited, Some(c)) if c != 0 => bail!("the container exited with {c}; see `vcontainer logs --session {name}`"),
                _ => {}
            }
            for p in &s.ports {
                println!("  port {p}: http://{}:{p}", s.ip);
            }
            return Ok(());
        }
    }
}

/// Signs and sends a launch, saved first so that a lost response can be
/// resumed; returns the launch event payload it must produce.
fn launch(s: &mut Session, owner: &crypto::SigningKey, boot: &BootInfo, args: LaunchArgs) -> Result<Vec<u8>> {
    let image = args.image.context("IMAGE is required")?;
    let r = ImageRef::parse(&image)?;
    let token = if r.is_google() { Some(gcp::access_token()?) } else { None };
    let mut reg = Registry::new(&r, token.clone());
    let digest = reg.resolve().with_context(|| format!("resolving {image}"))?;
    reg.image_manifest(&digest).with_context(|| format!("{image} has no usable linux/amd64 image"))?;
    let mut files = BTreeMap::new();
    for spec in &args.secrets {
        let (n, value) = parse_secret(spec)?;
        ensure!(files.insert(n.clone(), b64::encode(&value)).is_none(), "secret {n} given twice");
    }
    let config = Config {
        image: r.pinned(&digest),
        entrypoint: args.entrypoint.map(|e| vec![e]),
        cmd: (!args.cmd.is_empty()).then_some(args.cmd),
        env: args.env,
        workdir: args.workdir,
        user: args.user,
        secrets: files.keys().cloned().collect(),
        registry_token: token.is_some(),
        logs: if args.quiet { Logs::None } else { Logs::Serial },
    };
    let config_bytes = serde_json::to_vec(&config)?;
    let sealed = crypto::seal(boot, &serde_json::to_vec(&Secrets { files, registry_token: token })?)?;
    let issued_at = now();
    let payload = launch_payload(issued_at, &sealed, &config_bytes);
    let sig = crypto::sign(owner, &owner_message("launch", boot, &payload));
    s.policy.launch = Some(b64::encode(&payload));
    s.ports = args.ports;
    state::save_session(s)?;
    eprintln!("launching {}", config.image);
    verify::post(&s.url(), "/v1/launch", &LaunchRequest { issued_at, config: config_bytes, sealed, sig })?;
    Ok(payload)
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
    if policy.launch.is_some() && v.config.is_none() {
        bail!("the policy's launch is not measured (state {:?}): the VM restarted or the deploy never reached it", v.status.state);
    }
    report(&label, &v);
    if let Some(path) = export {
        std::fs::write(&path, serde_json::to_vec_pretty(&policy)?)?;
        println!("policy written to {} (give it to anyone who should verify this session)", path.display());
    }
    Ok(())
}

fn ls() -> Result<()> {
    let image = |l: &str| Some(Config::parse(launch_config(&b64::decode(l).ok()?).ok()?).ok()?.image);
    for s in state::sessions()? {
        println!("{:<20} {:<16} {:<15} {}", s.name, s.zone, s.ip, s.policy.launch.as_deref().and_then(image).unwrap_or_else(|| "-".into()));
    }
    Ok(())
}

fn logs(name: &str) -> Result<()> {
    let s = state::load_session(name)?;
    for line in gcp::serial(&s.project, &s.name, &s.zone)?.lines() {
        println!("{}", verify::untrusted(line.as_bytes()));
    }
    Ok(())
}

fn rm(name: &str) -> Result<()> {
    let s = state::load_session(name)?;
    gcp::delete(&s.project, &s.name, &s.zone)?;
    state::remove_session(name)?;
    println!("deleted session {name}");
    Ok(())
}

fn publish(dir: &std::path::Path, bucket: Option<String>) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct BuildInfo {
        commit: String,
        dirty: bool,
    }
    let project = gcp::project()?;
    let m = measure::measure(dir)?;
    measure::print(&m);
    let info: Option<BuildInfo> = std::fs::read(dir.join("build-info.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let commit = info.filter(|i| !i.dirty).map(|i| i.commit);
    if commit.is_none() {
        eprintln!("warning: {} was not built from a clean commit; the release records no commit", dir.display());
    }
    let image = format!("vcontainer-{}", &hex(&m.pe_sha384)[..12]);
    let bucket = bucket.unwrap_or_else(|| format!("{project}-vcontainer"));
    if gcp::describe(&["storage", "buckets", "describe", &format!("gs://{bucket}"), "--format=value(name)"])?.is_none() {
        gcp::gcloud(&["storage", "buckets", "create", &format!("gs://{bucket}"), "--location=us-central1", "--uniform-bucket-level-access"])?;
    }
    if gcp::describe(&["compute", "images", "describe", &image, "--format=value(name)"])?.is_none() {
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
        commit,
    })?;
    println!("published {image}; new sessions boot it");
    Ok(())
}
