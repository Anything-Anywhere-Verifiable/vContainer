//! vcontainer: run containers in verified Intel TDX confidential VMs on Google Cloud.
//!
//!   vcontainer run --session NAME            start a TDX VM and verify it (nothing secret sent yet)
//!   vcontainer deploy --session NAME IMAGE   verify again, then send the container (alias: pull)
//!   vcontainer verify --session NAME         verify at any time; third parties use --url/--policy

mod anim;
mod ccel;
mod endorse;
mod gcp;
mod measure;
mod state;
mod terminal;
mod verify;

use anyhow::{bail, ensure, Context, Result};
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vcontainer_core::registry::{ImageRef, Registry};
use vcontainer_core::*;

use anim::Kind;
use state::{Policy, Session};

/// Oldest acceptable Intel TCB evaluation data number.
const MIN_TCB_EVAL_NUM: u32 = 20;
/// Lowest acceptable security version of Google's TDX firmware (as of October 2026).
const MIN_FW_SVN: u64 = 2;

#[derive(Parser)]
#[command(name = "vcontainer", version, about = "Run containers in verified Intel TDX confidential VMs on Google Cloud")]
struct Cli {
    /// Include all verification claims and release metadata in the receipt.
    #[arg(long, global = true)]
    details: bool,
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
    /// Container port to open to the session's --allow-from range (set by `vcontainer run`).
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
        Cmd::Run { session, zone, machine_type, allow_from } => run(&session, &zone, &machine_type, allow_from, cli.details),
        Cmd::Deploy { session, resume, launch } => deploy(&session, (!resume).then_some(launch), cli.details),
        Cmd::Verify { session, url, policy, export_policy } => verify_cmd(session, url, policy, export_policy, cli.details),
        Cmd::Ls => ls(),
        Cmd::Logs { session } => logs(&session),
        Cmd::Rm { session } => rm(&session),
        Cmd::Measure { dir } => measure::measure(&dir).map(|m| measure::print(&m)),
        Cmd::Publish { dir, bucket } => publish(&dir, bucket),
    };
    if let Err(e) = result {
        use std::io::IsTerminal;
        if std::io::stderr().is_terminal() && !cli.details {
            eprintln!("error: {e}");
            eprintln!("  Use --details to include the full verification error and event log.");
        } else { eprintln!("error: {e:#}"); }
        std::process::exit(1);
    }
}

fn run(name: &str, zone: &str, machine_type: &str, allow_from: Option<String>, details: bool) -> Result<()> {
    let prev = state::find_session(name)?;
    let expected_id = prev.as_ref().map(|s| s.policy.instance.id);
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
    let existing = gcp::instance(&project, name, &zone)?;
    if let Some(inst) = &existing { gcp::check_instance(inst, name, expected_id)?; }
    ensure!(existing.is_some() || expected_id.is_none(),
        "the registered instance {name} no longer exists; run `vcontainer rm --session {name}` to clean its old record before creating a new session");
    // Resolve all project metadata before creating a billable resource.
    let project_number = gcp::project_number(&project)?;
    let inst = match existing {
        Some(i) if i.status == "RUNNING" => i,
        Some(i) => anim::step(Kind::Wait, &format!("starting {name} (was {})", i.status), |_| format!("started {name}"), || {
            gcp::start(&project, name, &zone)?;
            let started = gcp::instance(&project, name, &zone)?.context("instance disappeared")?;
            gcp::check_instance(&started, name, expected_id)?;
            Ok(started)
        })?,
        None => {
            gcp::ensure_network(&project)?;
            let what = format!("TDX confidential VM {name}: {machine_type} in {zone}");
            anim::step(Kind::Wait, &format!("creating {what}"), |_| format!("created {what}"), || {
                gcp::create_instance(&project, name, &zone, machine_type, &release.image, &release.project, &owner)
            })?
        }
    };
    let instance = state::Instance { id: inst.id.parse()?, project_number, zone: zone.clone() };
    let ip = match inst.ip() {
        Some(ip) => ip,
        None => {
            let error = anyhow::anyhow!("instance has no external IP");
            return Err(if expected_id.is_none() { rollback_create(&project, name, &zone, instance.id, error) } else { error });
        }
    };
    let policy = Policy { tee: "tdx".into(), owner, release, instance, launch, min_tcb_eval_num: MIN_TCB_EVAL_NUM, min_fw_svn: MIN_FW_SVN };
    let mut s = Session { name: name.into(), project, zone, ip, source, ports, policy };
    if let Err(error) = state::save_session(&s) {
        // A directory-sync error can occur after the atomic rename. Keep a
        // fully written record usable by rm; roll back only an unrecorded VM.
        let recorded = state::find_session(name).ok().flatten().is_some_and(|saved| saved.policy.instance.id == s.policy.instance.id);
        return Err(if expected_id.is_none() && !recorded {
            rollback_create(&s.project, name, &s.zone, s.policy.instance.id, error)
        } else { error });
    }
    anim::step(Kind::Wait, "preparing the session firewall", |_| "session firewall ready".into(), || {
        gcp::ensure_firewall(&s.project, name, &s.ports, &s.source, s.policy.instance.id, expected_id.is_some())
    })?;
    let wait = format!("waiting for {} to boot and answer attestation", s.url());
    let (ev, nonce) = anim::step(Kind::Verify, &wait, |_| format!("{name} returned attestation evidence"), || wait_evidence(&s, Duration::from_secs(300)))?;
    let v = terminal::verify(&format!("session {name} / {} / {}", s.ip, s.zone), &ev, &nonce, &s.policy)?;
    if v.status.state == State::Waiting {
        // A fresh boot: whatever was launched before is gone.
        s.policy.launch = None;
        s.ports.clear();
        state::save_session(&s)?;
    }
    report(&format!("session {name} ({}, {})", s.ip, s.zone), &v, &s.policy, details);
    if v.status.state == State::Waiting {
        println!("\nsession {name} is ready. Deploy a container:\n  vcontainer deploy --session {name} IMAGE [-p PORT] [--secret NAME=@FILE]");
    }
    Ok(())
}

fn rollback_create(project: &str, name: &str, zone: &str, instance_id: u64, error: anyhow::Error) -> anyhow::Error {
    match gcp::delete_instance(project, name, zone, instance_id) {
        Ok(()) => error.context("new VM removed because its session could not be recorded"),
        Err(cleanup) => error.context(format!("new VM {name} (ID {instance_id}, project {project}, zone {zone}) could not be recorded or safely removed: {cleanup:#}; inspect this resource before retrying")),
    }
}

fn wait_evidence(s: &Session, timeout: Duration) -> Result<(Evidence, [u8; 32])> {
    let start = Instant::now();
    for i in 0.. {
        let nonce = verify::nonce();
        let last = match verify::fetch_evidence(&s.url(), &nonce) {
            Ok(ev) => return Ok((ev, nonce)),
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

fn report(label: &str, v: &verify::Verified, policy: &Policy, details: bool) {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() || details { terminal::receipt(v, policy, details); }
    // Keep the existing complete stdout report for redirected/scripted use.
    if std::io::stdout().is_terminal() { return; }
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
fn deploy(name: &str, args: Option<LaunchArgs>, details: bool) -> Result<()> {
    let mut s = state::load_session(name)?;
    let owner = state::owner_key()?;
    terminal::gate("The VM must match your policy before it receives the signed launch.");
    let v = verify_endpoint(&format!("{name} / before launch"), &s.url(), &s.policy)
        .context("deployment stopped before launch; no owner commit was sent")?;
    let image = v.config.as_ref().map_or("?", |c| c.image.as_str());
    let payload = match (v.status.state, args) {
        (State::Waiting, Some(args)) => anim::step(
            Kind::Sign,
            "resolving the image, sealing the secrets and sending the signed launch",
            |_| "sent the signed launch".into(),
            || launch(&mut s, &owner, &v.boot, args),
        )?,
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
    let v2 = verify_endpoint(&format!("{name} / launch recorded, pull not authorized"), &s.url(), &s.policy)?;
    ensure!(
        v2.status.state == State::Starting && v2.boot == v.boot && v2.config.is_some(),
        "the session did not measure our launch (state {:?})",
        v2.status.state
    );
    terminal::gate("Recorded launch matches. The owner can now authorize the image pull.");
    anim::step(Kind::Sign, "opening the ports and signing the commit", |_| "committed: the VM may pull the image".into(), || {
        // The cloud name may have been reused since the last command.
        let inst = gcp::instance(&s.project, name, &s.zone)?.context("session instance disappeared before commit")?;
        gcp::check_instance(&inst, name, Some(s.policy.instance.id))?;
        gcp::ensure_firewall(&s.project, name, &s.ports, &s.source, s.policy.instance.id, true)?;
        let commit = CommitRequest { sig: crypto::sign(&owner, &owner_message("commit", &v.boot, &payload)) };
        verify::post(&s.url(), "/v1/commit", &commit)
    })?;

    let (ev, nonce) = anim::step(Kind::Container, "the VM is pulling and starting the container", |_| "container settled; checking final attestation".into(), || {
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
                return Ok((ev, n));
            }
        }
    })?;
    let v = terminal::verify(&format!("{name} / final state"), &ev, &nonce, &s.policy)?;
    report(&format!("session {name} ({}, {})", s.ip, s.zone), &v, &s.policy, details);
    match (v.status.state, v.status.exit_code) {
        (State::Failed, _) => bail!("the container did not start; see `vcontainer logs --session {name}`"),
        (State::Exited, Some(c)) if c != 0 => bail!("the container exited with {c}; see `vcontainer logs --session {name}`"),
        _ => {}
    }
    for p in &s.ports {
        println!("  port {p}: http://{}:{p}", s.ip);
    }
    Ok(())
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
    verify::post(&s.url(), "/v1/launch", &LaunchRequest { issued_at, config: config_bytes, sealed, sig })?;
    Ok(payload)
}

fn verify_endpoint(label: &str, url: &str, policy: &Policy) -> Result<verify::Verified> {
    let nonce = verify::nonce();
    let ev = anim::step(Kind::Verify, "requesting a fresh attestation quote", |_| "fresh evidence received".into(), || verify::fetch_evidence(url, &nonce))?;
    terminal::verify(label, &ev, &nonce, policy)
}

fn verify_cmd(session: Option<String>, url: Option<String>, policy: Option<PathBuf>, export: Option<PathBuf>, details: bool) -> Result<()> {
    let (url, policy, label) = match (session, url, policy) {
        (Some(name), None, None) => {
            let s = state::load_session(&name)?;
            (s.url(), s.policy, name)
        }
        (None, Some(url), Some(p)) => (url.clone(), serde_json::from_slice(&std::fs::read(&p)?)?, url),
        _ => bail!("use --session NAME, or --url URL --policy FILE"),
    };
    let v = verify_endpoint(&label, &url, &policy)?;
    if policy.launch.is_some() && v.config.is_none() {
        bail!("the policy's launch is not measured (state {:?}): the VM restarted or the deploy never reached it", v.status.state);
    }
    report(&label, &v, &policy, details);
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
    anim::step(Kind::Wait, &format!("deleting VM {name} and its firewall rule"), |_| format!("deleted session {name}"), || {
        gcp::delete(&s.project, &s.name, &s.zone, s.policy.instance.id)
    })?;
    state::remove_session(name)?;
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
        anim::step(Kind::Image, &format!("uploading the boot image to {object}"), |_| format!("uploaded {object}"), || {
            gcp::gcloud(&["storage", "cp", dir.join("vcontainer.tar.gz").to_str().unwrap(), &object])
        })?;
        anim::step(Kind::Image, &format!("creating GCE image {image}"), |_| format!("created GCE image {image}"), || {
            gcp::gcloud(&[
                "compute", "images", "create", &image,
                &format!("--source-uri={object}"),
                "--guest-os-features=UEFI_COMPATIBLE,GVNIC,TDX_CAPABLE",
                "--storage-location=us-central1",
                "--labels=vcontainer=image",
            ])
        })?;
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
