//! Google Cloud operations, done by running `gcloud` (no API client needed).

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::process::Command;
use std::time::Duration;

pub const NETWORK: &str = "vcontainer";

pub fn gcloud(args: &[&str]) -> Result<String> {
    let out = Command::new("gcloud").args(args).output().context("running gcloud (is the Google Cloud SDK installed?)")?;
    if !out.status.success() {
        bail!("gcloud {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

pub fn project() -> Result<String> {
    let p = gcloud(&["config", "get-value", "project"])?;
    if p.is_empty() || p == "(unset)" {
        bail!("no default project: run `gcloud config set project PROJECT`");
    }
    Ok(p)
}

pub fn project_number(project: &str) -> Result<u64> {
    Ok(gcloud(&["projects", "describe", project, "--format=value(projectNumber)"])?.parse()?)
}

pub fn access_token() -> Result<String> {
    gcloud(&["auth", "print-access-token"])
}

#[derive(Deserialize)]
pub struct Instance {
    pub id: String,
    pub status: String,
    #[serde(rename = "networkInterfaces", default)]
    nics: Vec<Nic>,
}

#[derive(Deserialize)]
struct Nic {
    #[serde(rename = "accessConfigs", default)]
    access: Vec<Access>,
}

#[derive(Deserialize)]
struct Access {
    #[serde(rename = "natIP")]
    ip: Option<String>,
}

impl Instance {
    pub fn ip(&self) -> Option<String> {
        self.nics.first()?.access.first()?.ip.clone()
    }
}

pub fn instance(name: &str, zone: &str) -> Result<Option<Instance>> {
    let out = Command::new("gcloud")
        .args(["compute", "instances", "describe", name, "--zone", zone, "--format=json"])
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("was not found") {
            return Ok(None);
        }
        bail!("gcloud compute instances describe {name}: {}", err.trim());
    }
    Ok(Some(serde_json::from_slice(&out.stdout)?))
}

pub fn ensure_network() -> Result<()> {
    if gcloud(&["compute", "networks", "describe", NETWORK, "--format=value(name)"]).is_err() {
        eprintln!("creating network {NETWORK}");
        gcloud(&["compute", "networks", "create", NETWORK, "--subnet-mode=auto"])?;
    }
    Ok(())
}

/// One firewall rule per session: the API port plus the container's ports,
/// open only to `source`.
pub fn ensure_firewall(session: &str, ports: &[u16], source: &str) -> Result<()> {
    let rule = format!("vcontainer-{session}");
    let allow: Vec<String> = std::iter::once(vcontainer_core::PORT).chain(ports.iter().copied()).map(|p| format!("tcp:{p}")).collect();
    let allow = allow.join(",");
    if gcloud(&["compute", "firewall-rules", "describe", &rule, "--format=value(name)"]).is_ok() {
        gcloud(&["compute", "firewall-rules", "update", &rule, &format!("--allow={allow}"), &format!("--source-ranges={source}")])?;
    } else {
        gcloud(&[
            "compute", "firewall-rules", "create", &rule,
            &format!("--network={NETWORK}"),
            &format!("--allow={allow}"),
            &format!("--source-ranges={source}"),
            &format!("--target-tags={rule}"),
        ])?;
    }
    Ok(())
}

pub fn create_instance(name: &str, zone: &str, machine_type: &str, image: &str, image_project: &str, owner: &str) -> Result<()> {
    gcloud(&[
        "compute", "instances", "create", name,
        &format!("--zone={zone}"),
        &format!("--machine-type={machine_type}"),
        "--confidential-compute-type=TDX",
        "--maintenance-policy=TERMINATE",
        &format!("--image={image}"),
        &format!("--image-project={image_project}"),
        "--boot-disk-type=pd-balanced",
        "--boot-disk-size=10GB",
        &format!("--network-interface=network={NETWORK},nic-type=GVNIC,stack-type=IPV4_ONLY"),
        "--no-shielded-secure-boot",
        "--shielded-vtpm",
        "--no-service-account",
        "--no-scopes",
        &format!("--tags=vcontainer-{name}"),
        "--labels=vcontainer=session",
        &format!("--metadata={}={owner}", vcontainer_core::OWNER_METADATA_KEY),
    ])?;
    Ok(())
}

pub fn start(name: &str, zone: &str) -> Result<()> {
    gcloud(&["compute", "instances", "start", name, &format!("--zone={zone}")]).map(|_| ())
}

pub fn delete(name: &str, zone: &str) -> Result<()> {
    if instance(name, zone)?.is_some() {
        gcloud(&["compute", "instances", "delete", name, &format!("--zone={zone}"), "--quiet"])?;
    }
    let rule = format!("vcontainer-{name}");
    if gcloud(&["compute", "firewall-rules", "describe", &rule, "--format=value(name)"]).is_ok() {
        gcloud(&["compute", "firewall-rules", "delete", &rule, "--quiet"])?;
    }
    Ok(())
}

pub fn serial(name: &str, zone: &str) -> Result<String> {
    gcloud(&["compute", "instances", "get-serial-port-output", name, &format!("--zone={zone}")])
}

/// This machine's public IPv4 address, for firewall rules.
pub fn my_ip() -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    let ip = agent.get("https://checkip.amazonaws.com").call()?.body_mut().read_to_string()?;
    let ip = ip.trim().to_string();
    ip.parse::<std::net::Ipv4Addr>().context("could not determine this machine's public IPv4 address (use --allow-from)")?;
    Ok(ip)
}
