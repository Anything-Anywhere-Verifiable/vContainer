//! Google Cloud operations, done by running `gcloud` (no API client needed).

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::process::Command;

pub const NETWORK: &str = "vcontainer";

pub fn gcloud(args: &[&str]) -> Result<String> {
    let out = Command::new("gcloud").args(args).output().context("running gcloud (is the Google Cloud SDK installed?)")?;
    if !out.status.success() {
        bail!("gcloud {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A `gcloud ... describe`: None only if the resource does not exist; any
/// other failure (auth, network, quota) is an error.
pub fn describe(args: &[&str]) -> Result<Option<String>> {
    match gcloud(args) {
        Ok(out) => Ok(Some(out)),
        Err(e) if e.to_string().contains("not found") => Ok(None),
        Err(e) => Err(e),
    }
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

/// A freshly minted access token (about an hour left), for a sealed pull.
pub fn access_token() -> Result<String> {
    gcloud(&["config", "config-helper", "--force-auth-refresh", "--format=value(credential.access_token)"])
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

pub fn instance(project: &str, name: &str, zone: &str) -> Result<Option<Instance>> {
    let p = format!("--project={project}");
    Ok(describe(&["compute", "instances", "describe", name, "--zone", zone, "--format=json", &p])?.map(|j| serde_json::from_str(&j)).transpose()?)
}

pub fn ensure_network(project: &str) -> Result<()> {
    let p = format!("--project={project}");
    if describe(&["compute", "networks", "describe", NETWORK, "--format=value(name)", &p])?.is_none() {
        gcloud(&["compute", "networks", "create", NETWORK, "--subnet-mode=auto", &p])?;
    }
    Ok(())
}

/// One firewall rule per session: the API port plus the container's ports,
/// open only to `source`.
pub fn ensure_firewall(project: &str, session: &str, ports: &[u16], source: &str) -> Result<()> {
    let p = format!("--project={project}");
    let rule = format!("vcontainer-{session}");
    let allow: Vec<String> = std::iter::once(vcontainer_core::PORT).chain(ports.iter().copied()).map(|p| format!("tcp:{p}")).collect();
    let allow = allow.join(",");
    if describe(&["compute", "firewall-rules", "describe", &rule, "--format=value(name)", &p])?.is_some() {
        gcloud(&["compute", "firewall-rules", "update", &rule, &format!("--allow={allow}"), &format!("--source-ranges={source}"), &p])?;
    } else {
        gcloud(&[
            "compute", "firewall-rules", "create", &rule, &p,
            &format!("--network={NETWORK}"),
            &format!("--allow={allow}"),
            &format!("--source-ranges={source}"),
            &format!("--target-tags={rule}"),
        ])?;
    }
    Ok(())
}

pub fn create_instance(project: &str, name: &str, zone: &str, machine_type: &str, image: &str, image_project: &str, owner: &str) -> Result<()> {
    gcloud(&[
        "compute", "instances", "create", name,
        &format!("--project={project}"),
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

pub fn start(project: &str, name: &str, zone: &str) -> Result<()> {
    gcloud(&["compute", "instances", "start", name, &format!("--zone={zone}"), &format!("--project={project}")]).map(|_| ())
}

pub fn delete(project: &str, name: &str, zone: &str) -> Result<()> {
    let p = format!("--project={project}");
    if instance(project, name, zone)?.is_some() {
        gcloud(&["compute", "instances", "delete", name, &format!("--zone={zone}"), "--quiet", &p])?;
    }
    let rule = format!("vcontainer-{name}");
    if describe(&["compute", "firewall-rules", "describe", &rule, "--format=value(name)", &p])?.is_some() {
        gcloud(&["compute", "firewall-rules", "delete", &rule, "--quiet", &p])?;
    }
    Ok(())
}

pub fn serial(project: &str, name: &str, zone: &str) -> Result<String> {
    gcloud(&["compute", "instances", "get-serial-port-output", name, &format!("--zone={zone}"), &format!("--project={project}")])
}

/// This machine's public IPv4 address, for firewall rules.
pub fn my_ip() -> Result<String> {
    let ip = String::from_utf8_lossy(&crate::verify::http("https://checkip.amazonaws.com", None)?).trim().to_string();
    ip.parse::<std::net::Ipv4Addr>().context("could not determine this machine's public IPv4 address (use --allow-from)")?;
    Ok(ip)
}
