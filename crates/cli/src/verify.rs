//! Verification of a TDX session's evidence against a policy.
//!
//! What a PASS means: Intel signed a quote for a TD running Google firmware
//! that Google signed (MRTD), which booted exactly the published vcontainer
//! PE (RTMR1) with nothing else in RTMR2, whose /init recorded the ledger in
//! RTMR3: a fresh boot key, this owner's key, and (once launched) the
//! policy's exact launch, before pulling anything. The quote answers our
//! nonce and binds the reported status. ACPI and other firmware configuration
//! in RTMR0 are trusted (default GCE configuration), not appraised.

use anyhow::{bail, ensure, Context, Result};
use dcap_qvl::quote::Report;
use sha2::{Digest, Sha384};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vcontainer_core::*;

use crate::state::Policy;

/// What verification established.
pub struct Verified {
    pub status: Status,
    pub boot: BootInfo,
    pub config: Option<Config>,
    pub claims: Vec<String>,
}

pub fn nonce() -> [u8; 32] {
    crypto::random()
}

/// Text from the network or the serial console, made safe to print as one
/// line: no control characters (newlines and ESC included), no bidi controls
/// that reorder what the terminal shows.
pub(crate) fn untrusted(b: &[u8]) -> String {
    let bidi = |c: char| matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    String::from_utf8_lossy(b).chars().filter(|&c| !c.is_control() && !bidi(c)).collect()
}

/// A GET, or a POST of `json`; any status but 200 is an error.
pub fn http(url: &str, json: Option<Vec<u8>>) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).http_status_as_error(false).build().into();
    let mut r = match json {
        Some(b) => agent.post(url).header("Content-Type", "application/json").send(b)?,
        None => agent.get(url).call()?,
    };
    let body = r.body_mut().with_config().limit(32 << 20).read_to_vec()?;
    let status = r.status();
    ensure!(status == 200, "{url}: HTTP {status}: {}", untrusted(&body[..body.len().min(300)]));
    Ok(body)
}

pub fn fetch_evidence(url: &str, nonce: &[u8; 32]) -> Result<Evidence> {
    let body = http(&format!("{url}/v1/evidence?nonce={}", hex(nonce)), None)
        .with_context(|| format!("no answer from {url} (if the VM restarted, `vcontainer run --session NAME` reconnects)"))?;
    Ok(serde_json::from_slice(&body)?)
}

pub fn post(url: &str, path: &str, body: &impl serde::Serialize) -> Result<()> {
    http(&format!("{url}{path}"), Some(serde_json::to_vec(body)?)).map(drop)
}

pub fn verify(ev: &Evidence, nonce: &[u8; 32], policy: &Policy) -> Result<Verified> {
    let mut claims = Vec::new();
    ensure!(ev.v == 1 && ev.tee == "tdx" && policy.tee == "tdx", "unsupported evidence or policy (only TDX, version 1)");

    // 1. Intel: quote signature, certificate chain and TCB status.
    let collateral = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async { dcap_qvl::collateral::CollateralClient::with_default_http(dcap_qvl::collateral::INTEL_PCS_URL)?.fetch(&ev.quote).await })
        .context("fetching Intel collateral")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let qv = dcap_qvl::verify::verify(&ev.quote, &collateral, now).context("TD quote verification")?;
    let eval = |json: &str| -> Result<u64> { Ok(serde_json::from_str::<serde_json::Value>(json)?["tcbEvaluationDataNumber"].as_u64().unwrap_or(0)) };
    let eval_num = eval(&collateral.tcb_info)?.min(eval(&collateral.qe_identity)?);
    ensure!(qv.status == "UpToDate", "TCB status is {} (advisories {:?})", qv.status, qv.advisory_ids);
    ensure!(qv.advisory_ids.is_empty(), "TCB advisories {:?}", qv.advisory_ids);
    ensure!(eval_num >= policy.min_tcb_eval_num as u64, "TCB evaluation data number {eval_num} < {}", policy.min_tcb_eval_num);
    let td = match &qv.report {
        Report::TD10(r) => *r,
        Report::TD15(r) => r.base,
        _ => bail!("not a TD quote"),
    };
    claims.push(format!("Intel TDX quote: signature valid, TCB UpToDate (evaluation {eval_num})"));

    // 2. Freshness and reported status.
    ensure!(td.report_data == report_data(nonce, &ev.status), "REPORTDATA does not match our nonce and the reported status");
    let status: Status = serde_json::from_slice(&ev.status)?;
    claims.push(format!("fresh quote for our nonce; state {:?}", status.state));

    // 3. TD configuration fixed by GCE.
    ensure!(u64::from_le_bytes(td.td_attributes) == 0x1000_0000, "unexpected TD attributes {}", hex(&td.td_attributes));
    ensure!(td.mr_config_id == [0; 48] && td.mr_owner_config == [0; 48], "MRCONFIGID/MROWNERCONFIG are set");
    claims.push(format!("TD attributes: debug off (0x10000000), XFAM {:#x}", u64::from_le_bytes(td.xfam)));
    let i = &policy.instance;
    let pzid = format!("{{\"instanceId\":{},\"numericalProjectId\":{},\"zone\":{}}}", i.id, i.project_number, serde_json::to_string(&i.zone)?);
    ensure!(td.mr_owner[..] == Sha384::digest(pzid.as_bytes())[..], "MROWNER is not GCE instance {} in {}", i.id, i.zone);
    claims.push(format!("GCE instance {} in {} (MROWNER)", i.id, i.zone));

    // 4. Firmware: Google-signed launch endorsement for this MRTD.
    let svn = crate::endorse::check_mrtd(&td.mr_td)?;
    ensure!(svn >= policy.min_fw_svn, "firmware security version {svn} < {}", policy.min_fw_svn);
    claims.push(format!("firmware: MRTD {}… endorsed by Google (svn {svn})", &hex(&td.mr_td)[..16]));
    crate::endorse::check_host(&qv.ppid)?;
    claims.push("host: a Google Cloud machine (PPID in Google's host registry)".into());

    // 5. Boot chain: exactly the published PE, nothing in RTMR2.
    // The accepted RTMR1 is derived from the release's PE and GPT digests, so
    // the Authenticode we print is the one that was checked.
    let r = &policy.release;
    let expected = crate::measure::rtmr1(&unhex_array(&r.pe_authenticode_sha384)?, &unhex_array(&r.gpt_event_sha384)?);
    if !expected.contains(&td.rt_mr1) {
        bail!("RTMR1 {} is not the expected boot image:\n{}", hex(&td.rt_mr1), crate::ccel::describe(&ev.ccel, 2));
    }
    ensure!(td.rt_mr2 == [0; 48], "RTMR2 is not empty:\n{}", crate::ccel::describe(&ev.ccel, 3));
    claims.push(format!(
        "boot image: kernel+initramfs+cmdline+/init with Authenticode {} (release {}{})",
        r.pe_authenticode_sha384,
        r.image,
        r.commit.as_deref().map(|c| format!(", commit {c}")).unwrap_or_default()
    ));

    // 6. Ledger in RTMR3.
    ensure!(replay(ev.ledger.iter().map(Event::digest)) == td.rt_mr3, "ledger does not replay to RTMR3");
    let names: Vec<&str> = ev.ledger.iter().map(|e| e.name.as_str()).collect();
    ensure!(names.len() >= 2 && [EV_BOOT, EV_OWNER, EV_LAUNCH, EV_EXEC].starts_with(&names), "unexpected ledger {names:?}");
    let boot = BootInfo::decode(&ev.ledger[0].payload)?;
    ensure!(ev.ledger[1].payload == policy.owner.as_bytes(), "the session's owner key is not ours");
    claims.push("ledger: fresh boot key, our owner key".into());
    let config = match ev.ledger.get(2) {
        Some(e) => {
            if let Some(want) = &policy.launch {
                ensure!(e.payload == b64::decode(want)?, "ledger event 2 is not the expected launch");
            }
            let c = Config::parse(launch_config(&e.payload)?)?;
            claims.push(format!("launch measured before pull: {}", c.image));
            Some(c)
        }
        None => None,
    };
    if let Some(e) = ev.ledger.get(3) {
        claims.push(format!("container sealed: manifest {}", String::from_utf8_lossy(&e.payload)));
    }
    Ok(Verified { status, boot, config, claims })
}
