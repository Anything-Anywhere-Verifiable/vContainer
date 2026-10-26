//! Pull the committed image by digest, unpack it layer by layer into
//! /newroot, seal the ledger and start the container.

use anyhow::{bail, ensure, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use vcontainer_core::registry::{ImageRef, Registry, RunConfig};
use vcontainer_core::{Config, Event, Logs, Secrets, State, EV_EXEC};

use crate::app::App;
use crate::run::RunSpec;

pub const ROOT: &str = "/newroot";
pub const LAYER: &str = "/run/layer";

pub fn run(app: Arc<App>, config: Config, secrets: Secrets) {
    if let Err(e) = deploy(&app, config, secrets) {
        app.set_state(State::Failed, None, Some(format!("{e:#}")));
    }
}

fn deploy(app: &App, config: Config, secrets: Secrets) -> Result<()> {
    let image = ImageRef::parse(&config.image)?;
    let mut reg = Registry::new(&image, secrets.registry_token);

    crate::sys::log(&format!("pulling {}", config.image));
    let (manifest_digest, manifest) = reg.image_manifest(&image.reference)?;
    let image_config = reg.config(&manifest.config)?;

    crate::sys::mount("tmpfs", ROOT, "tmpfs", 0, Some("mode=0755,size=90%"))?;
    for (i, layer) in manifest.layers.iter().enumerate() {
        crate::sys::log(&format!("layer {}/{} {} ({} bytes)", i + 1, manifest.layers.len(), layer.digest, layer.size));
        for attempt in 1..=3 {
            let mut f = fs::File::create(LAYER)?;
            match reg.blob(&layer.digest, layer.size, &mut f) {
                Ok(()) => break,
                Err(e) if attempt < 3 => crate::sys::log(&format!("retrying {}: {e:#}", layer.digest)),
                Err(e) => return Err(e),
            }
        }
        helper("unpack", b"", |word, _| bail!("unexpected {word:?}")).with_context(|| format!("layer {}", layer.digest))?;
        fs::remove_file(LAYER)?;
    }

    let spec = run_spec(&config, image_config.config, secrets.files)?;
    app.measure(Event::new(EV_EXEC, manifest_digest.into_bytes()))?;
    helper("run", &serde_json::to_vec(&spec)?, |word, rest| {
        match word {
            "started" => {
                crate::sys::log(&format!("container started: {}", spec.argv.join(" ")));
                app.set_state(State::Running, None, None);
            }
            "exited" => {
                crate::sys::log(&format!("container exited with {rest}"));
                app.set_state(State::Exited, Some(rest.parse()?), None);
            }
            _ => bail!("unexpected {word:?}"),
        }
        Ok(())
    })
}

/// Merges the launch config over the image config as `docker run` does: a
/// configured entrypoint drops the image's Cmd, and launch values win.
fn run_spec(c: &Config, rc: RunConfig, files: BTreeMap<String, String>) -> Result<RunSpec> {
    let (entry, cmd) = match (&c.entrypoint, &c.cmd) {
        (Some(e), cmd) => (e.clone(), cmd.clone().unwrap_or_default()),
        (None, Some(cmd)) => (rc.entrypoint.unwrap_or_default(), cmd.clone()),
        (None, None) => (rc.entrypoint.unwrap_or_default(), rc.cmd.unwrap_or_default()),
    };
    let argv: Vec<String> = entry.into_iter().chain(cmd).collect();
    ensure!(!argv.is_empty(), "no command: the image has no Entrypoint/Cmd and none was given");
    let mut env = BTreeMap::new();
    for kv in rc.env.iter().flatten().chain(&c.env) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        env.insert(k, v);
    }
    Ok(RunSpec {
        argv,
        env: env.into_iter().map(|(k, v)| format!("{k}={v}")).collect(),
        cwd: c.workdir.clone().or(rc.workdir).filter(|w| !w.is_empty()).unwrap_or_else(|| "/".into()),
        user: c.user.clone().or(rc.user).unwrap_or_default(),
        files,
        serial: c.logs == Logs::Serial,
    })
}

/// Runs `/init --stage <stage>` with `input` on stdin. Its stdout carries
/// progress lines, handed to `progress` as (word, rest), then `ok` or
/// `failed <reason>`.
fn helper(stage: &str, input: &[u8], mut progress: impl FnMut(&str, &str) -> Result<()>) -> Result<()> {
    let mut child = Command::new("/init")
        .args(["--stage", stage])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(input)?;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line?;
        let (word, rest) = line.split_once(' ').unwrap_or((&line, ""));
        match word {
            "ok" => return Ok(()),
            "failed" => bail!("{stage}: {rest}"),
            _ => progress(word, rest)?,
        }
    }
    bail!("{stage}: helper ended without a result")
}
