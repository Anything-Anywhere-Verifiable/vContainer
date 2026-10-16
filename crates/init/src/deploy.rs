//! Pull the committed image by digest, unpack it layer by layer into
//! /newroot, seal the ledger and start the container.

use anyhow::{bail, ensure, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use vcontainer_core::registry::{ImageConfig, ImageRef, Manifest, Registry};
use vcontainer_core::{Event, Logs, State, EV_EXEC};

use crate::app::{extend, App};
use crate::run::RunSpec;

pub const ROOT: &str = "/newroot";
const LAYER: &str = "/run/layer";

pub fn run(app: Arc<App>) {
    if let Err(e) = deploy(&app) {
        app.set_state(State::Failed, None, Some(format!("{e:#}")));
    }
}

fn deploy(app: &Arc<App>) -> Result<()> {
    let (config, token, files) = {
        let mut g = app.inner.lock().unwrap();
        let l = g.launch.as_mut().context("no launch")?;
        let files = std::mem::take(&mut l.secrets.files);
        (l.config.clone(), l.secrets.registry_token.take(), files)
    };
    let image = ImageRef::parse(&config.image)?;
    let digest = image.digest.clone().context("image not pinned")?;
    let creds = token.map(|t| ("oauth2accesstoken".to_string(), t));
    let mut reg = Registry::new(&image, creds);

    crate::sys::log(&format!("pulling {}", config.image));
    let (body, _) = reg.manifest(&digest)?;
    let mut manifest: Manifest = serde_json::from_slice(&body)?;
    let mut manifest_digest = digest.clone();
    if manifest.is_index() {
        manifest_digest = manifest.linux_amd64()?.digest.clone();
        manifest = serde_json::from_slice(&reg.manifest(&manifest_digest)?.0)?;
    }
    let cfg = manifest.config.as_ref().context("manifest has no config")?;
    let mut raw = Vec::new();
    reg.blob(&cfg.digest, cfg.size, &mut raw)?;
    let image_config: ImageConfig = serde_json::from_slice(&raw)?;

    crate::sys::mount("tmpfs", ROOT, "tmpfs", 0, Some("mode=0755,size=90%"))?;
    for (i, layer) in manifest.layers.iter().enumerate() {
        crate::sys::log(&format!("layer {}/{} {} ({} bytes)", i + 1, manifest.layers.len(), layer.digest, layer.size));
        {
            let mut f = fs::File::create(LAYER)?;
            reg.blob(&layer.digest, layer.size, &mut f)?;
        }
        unpack(LAYER).with_context(|| format!("unpack {}", layer.digest))?;
        fs::remove_file(LAYER)?;
    }

    let rc = image_config.config;
    let (argv, env, cwd) = command(&config, &rc)?;
    let (uid, gid, home) = user(config.user.as_deref().or(rc.user.as_deref()).unwrap_or(""))?;
    let env = with_defaults(env, &home);
    prepare_root(uid, gid, &files)?;

    {
        let mut g = app.inner.lock().unwrap();
        extend(&mut g, Event::new(EV_EXEC, manifest_digest.into_bytes()))?;
    }
    let spec = RunSpec { argv, env, cwd, uid, gid, serial: config.logs == Logs::Serial };
    start(app, &spec)
}

/// Runs the unpack helper (chrooted into ROOT) with the layer as stdin.
fn unpack(layer: &str) -> Result<()> {
    let mut child = Command::new("/init")
        .args(["--stage", "unpack", ROOT])
        .stdin(fs::File::open(layer)?)
        .stdout(Stdio::piped())
        .spawn()?;
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out)?;
    ensure!(out.trim() == "ok", "unpack helper failed");
    Ok(())
}

/// Docker semantics: a configured entrypoint drops the image's Cmd.
fn command(c: &vcontainer_core::Config, rc: &vcontainer_core::registry::RunConfig) -> Result<(Vec<String>, Vec<String>, String)> {
    let (entry, cmd) = match (&c.entrypoint, &c.cmd) {
        (Some(e), cmd) => (e.clone(), cmd.clone().unwrap_or_default()),
        (None, Some(cmd)) => (rc.entrypoint.clone().unwrap_or_default(), cmd.clone()),
        (None, None) => (rc.entrypoint.clone().unwrap_or_default(), rc.cmd.clone().unwrap_or_default()),
    };
    let argv: Vec<String> = entry.into_iter().chain(cmd).collect();
    ensure!(!argv.is_empty(), "no command: the image has no Entrypoint/Cmd and none was given");
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    for kv in rc.env.iter().flatten().chain(c.env.iter()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        env.insert(k.to_string(), v.to_string());
    }
    let env = env.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    let cwd = c.workdir.clone().or(rc.workdir.clone()).filter(|w| !w.is_empty()).unwrap_or_else(|| "/".into());
    Ok((argv, env, cwd))
}

fn with_defaults(mut env: Vec<String>, home: &str) -> Vec<String> {
    let has = |env: &Vec<String>, k: &str| env.iter().any(|e| e.starts_with(&format!("{k}=")));
    if !has(&env, "PATH") {
        env.push("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
    }
    if !has(&env, "HOME") {
        env.push(format!("HOME={home}"));
    }
    if !has(&env, "HOSTNAME") {
        env.push("HOSTNAME=vcontainer".into());
    }
    env
}

/// Resolves `user[:group]` against the image's /etc/passwd and /etc/group.
fn user(spec: &str) -> Result<(u32, u32, String)> {
    let read = |f: &str| fs::read_to_string(format!("{ROOT}/etc/{f}")).unwrap_or_default();
    let (passwd, group) = (read("passwd"), read("group"));
    let (u, g) = spec.split_once(':').map_or((spec, None), |(u, g)| (u, Some(g)));
    let entry = passwd.lines().map(|l| l.split(':').collect::<Vec<_>>()).find(|f| {
        f.len() >= 6 && (f[0] == u || (u.parse::<u32>().is_ok() && f[2] == u) || (u.is_empty() && f[2] == "0"))
    });
    let uid = match (u.parse::<u32>(), &entry) {
        (Ok(n), _) => n,
        (_, Some(f)) => f[2].parse()?,
        _ if u.is_empty() => 0,
        _ => bail!("user {u:?} not found in the image's /etc/passwd"),
    };
    let gid = match g {
        Some(g) => match g.parse::<u32>() {
            Ok(n) => n,
            Err(_) => group
                .lines()
                .find_map(|l| {
                    let f: Vec<_> = l.split(':').collect();
                    (f.len() >= 3 && f[0] == g).then(|| f[2].parse().ok()).flatten()
                })
                .with_context(|| format!("group {g:?} not found"))?,
        },
        None => entry.as_ref().and_then(|f| f[3].parse().ok()).unwrap_or(0),
    };
    let home = entry.as_ref().map(|f| f[5].to_string()).unwrap_or_else(|| "/".into());
    Ok((uid, gid, home))
}

fn prepare_root(uid: u32, gid: u32, files: &BTreeMap<String, String>) -> Result<()> {
    fs::create_dir_all(format!("{ROOT}/etc"))?;
    fs::copy("/etc/resolv.conf", format!("{ROOT}/etc/resolv.conf"))?;
    fs::write(format!("{ROOT}/etc/hosts"), "127.0.0.1\tlocalhost vcontainer\n")?;
    fs::write(format!("{ROOT}/etc/hostname"), "vcontainer\n")?;
    let dir = format!("{ROOT}/run/secrets");
    fs::create_dir_all(&dir)?;
    for (name, b64) in files {
        let path = format!("{dir}/{name}");
        let mut f = fs::File::create(&path)?;
        f.write_all(&vcontainer_core::b64::decode(b64)?)?;
        std::os::unix::fs::chown(&path, Some(uid), Some(gid))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400))?;
    }
    Ok(())
}

/// Starts the run helper and follows its reports: started / failed / exited.
fn start(app: &Arc<App>, spec: &RunSpec) -> Result<()> {
    let mut child = Command::new("/init")
        .args(["--stage", "run"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(&serde_json::to_vec(spec)?)?;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line?;
        let (word, rest) = line.split_once(' ').unwrap_or((&line, ""));
        match word {
            "started" => {
                crate::sys::log(&format!("container started: {}", spec.argv.join(" ")));
                app.set_state(State::Running, None, None);
            }
            "failed" => bail!("container failed to start: {rest}"),
            "exited" => {
                let code = rest.parse().unwrap_or(-1);
                crate::sys::log(&format!("container exited with {code}"));
                app.set_state(State::Exited, Some(code), None);
                return Ok(());
            }
            _ => {}
        }
    }
    bail!("run helper ended unexpectedly")
}
