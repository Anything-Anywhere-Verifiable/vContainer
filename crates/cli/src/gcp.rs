//! Google Cloud operations, done by running `gcloud` (no API client needed).

use anyhow::{bail, ensure, Context, Result};
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

/// A name can be reused after deletion. Only the immutable ID in our saved
/// session authorizes changing an existing instance.
pub fn check_instance(inst: &Instance, name: &str, expected_id: Option<u64>) -> Result<()> {
    let expected = expected_id.with_context(|| format!("instance {name} already exists but is not registered as this session; choose a different session name"))?;
    ensure!(inst.id.parse::<u64>()? == expected,
        "instance {name} has ID {}, but this session owns ID {expected}; refusing to change or delete the replacement", inst.id);
    Ok(())
}

#[derive(Deserialize)]
struct Firewall {
    #[serde(default)]
    description: String,
    network: String,
    #[serde(rename = "targetTags", default)]
    target_tags: Vec<String>,
}

fn firewall_owner(session: &str, instance_id: u64) -> String {
    format!("vcontainer session {session}; instance {instance_id}")
}

/// Old releases did not write a marker. Migrate only their exact network/tag
/// scope and only while the saved instance ID has been checked by the caller.
fn check_firewall(rule: &Firewall, session: &str, instance_id: u64, allow_legacy: bool) -> Result<()> {
    let scoped = rule.network.ends_with(&format!("/networks/{NETWORK}"))
        && rule.target_tags == [format!("vcontainer-{session}")];
    let owned = rule.description == firewall_owner(session, instance_id)
        || (allow_legacy && rule.description.is_empty());
    ensure!(scoped && owned, "firewall vcontainer-{session} is not owned by session instance {instance_id}; refusing to change or delete it");
    Ok(())
}

fn firewall(project: &str, session: &str) -> Result<Option<Firewall>> {
    describe(&["compute", "firewall-rules", "describe", &format!("vcontainer-{session}"), "--format=json", &format!("--project={project}")])?
        .map(|j| serde_json::from_str(&j).context("parse firewall rule")).transpose()
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
pub fn ensure_firewall(project: &str, session: &str, ports: &[u16], source: &str, instance_id: u64, allow_legacy: bool) -> Result<()> {
    let p = format!("--project={project}");
    let rule = format!("vcontainer-{session}");
    let allow: Vec<String> = std::iter::once(vcontainer_core::PORT).chain(ports.iter().copied()).map(|p| format!("tcp:{p}")).collect();
    let allow = allow.join(",");
    let owner = format!("--description={}", firewall_owner(session, instance_id));
    if let Some(existing) = firewall(project, session)? {
        check_firewall(&existing, session, instance_id, allow_legacy)?;
        gcloud(&["compute", "firewall-rules", "update", &rule, &format!("--allow={allow}"), &format!("--source-ranges={source}"), &owner, &p])?;
    } else {
        gcloud(&[
            "compute", "firewall-rules", "create", &rule, &p,
            &format!("--network={NETWORK}"),
            &format!("--allow={allow}"),
            &format!("--source-ranges={source}"),
            &format!("--target-tags={rule}"),
            &owner,
        ])?;
    }
    Ok(())
}

pub fn create_instance(project: &str, name: &str, zone: &str, machine_type: &str, image: &str, image_project: &str, owner: &str) -> Result<Instance> {
    let raw = gcloud(&[
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
        "--format=json",
    ])?;
    let mut created: Vec<Instance> = serde_json::from_str(&raw).context("parse created instance")?;
    ensure!(created.len() == 1, "create did not return exactly one instance");
    Ok(created.remove(0))
}

pub fn start(project: &str, name: &str, zone: &str) -> Result<()> {
    gcloud(&["compute", "instances", "start", name, &format!("--zone={zone}"), &format!("--project={project}")]).map(|_| ())
}

pub fn delete(project: &str, name: &str, zone: &str, instance_id: u64) -> Result<()> {
    let p = format!("--project={project}");
    let inst = instance(project, name, zone)?;
    if let Some(inst) = &inst {
        check_instance(inst, name, Some(instance_id))?;
    }
    let rule = format!("vcontainer-{name}");
    let existing = firewall(project, name)?;
    if let Some(existing) = &existing {
        check_firewall(&existing, name, instance_id, inst.is_some())?;
        // Persist the legacy ownership proof while the matching VM still
        // exists, so a failed firewall delete can safely be retried later.
        if existing.description.is_empty() {
            gcloud(&["compute", "firewall-rules", "update", &rule, &format!("--description={}", firewall_owner(name, instance_id)), &p])?;
        }
    }
    if inst.is_some() {
        delete_instance(project, name, zone, instance_id)?;
    }
    if existing.is_some() {
        gcloud(&["compute", "firewall-rules", "delete", &rule, "--quiet", &p])?;
    }
    Ok(())
}

/// Also used to roll back a successful create before a session record exists.
/// No firewall is owned yet at that point, so do not touch any same-name rule.
pub fn delete_instance(project: &str, name: &str, zone: &str, instance_id: u64) -> Result<()> {
    if let Some(inst) = instance(project, name, zone)? {
        check_instance(&inst, name, Some(instance_id))?;
        gcloud(&["compute", "instances", "delete", name, &format!("--zone={zone}"), "--quiet", &format!("--project={project}")])?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reused_name_does_not_authorize_instance_mutation() {
        let inst: Instance = serde_json::from_value(serde_json::json!({"id":"42", "status":"RUNNING"})).unwrap();
        assert!(check_instance(&inst, "demo", None).is_err());
        assert!(check_instance(&inst, "demo", Some(41)).is_err());
        assert!(check_instance(&inst, "demo", Some(42)).is_ok());
    }

    #[test]
    fn firewall_ownership_requires_instance_and_narrow_scope() {
        let mut rule = Firewall { description: firewall_owner("demo", 42), network: "https://www.googleapis.com/compute/v1/projects/p/global/networks/vcontainer".into(), target_tags: vec!["vcontainer-demo".into()] };
        assert!(check_firewall(&rule, "demo", 42, false).is_ok());
        assert!(check_firewall(&rule, "demo", 43, true).is_err());
        rule.description.clear();
        assert!(check_firewall(&rule, "demo", 42, false).is_err());
        assert!(check_firewall(&rule, "demo", 42, true).is_ok());
        rule.target_tags.clear();
        assert!(check_firewall(&rule, "demo", 42, true).is_err());
        rule.target_tags = vec!["vcontainer-demo".into()];
        rule.network = "projects/p/global/networks/production".into();
        assert!(check_firewall(&rule, "demo", 42, true).is_err());
    }
}
