"""Exercise the built CLI's cloud ownership boundaries without Google Cloud.

Build first: cargo build --locked -p vcontainer
Run: python3 -m unittest discover -s scripts -p 'test_cloud_lifecycle.py' -v
VCONTAINER_TEST_BIN can select a different built CLI. No binary means a clear skip.

Every CLI subprocess gets an isolated HOME/state directory and a PATH containing
only our fake gcloud. Run cases stop before attestation. Endpoint strings are
deliberately invalid URLs, so a regression cannot contact a VM or resolve a host.
"""

import base64
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("VCONTAINER_TEST_BIN", REPO / "target/debug/vcontainer")).expanduser().resolve()
PROJECT = "lifecycle-test-project"
ZONE = "us-central1-a"
NAME = "lifecycle-test"
RULE = "vcontainer-" + NAME
OWNER_MARKER = f"vcontainer session {NAME}; instance 42"


# The fake is a separate executable, not a mock of Rust helper functions. It
# persists cloud state between gcloud subprocesses and even between CLI retries.
FAKE_GCLOUD = r'''
import json
import os
from pathlib import Path
import sys

state_path = Path(os.environ["FAKE_GCLOUD_STATE"])
log_path = Path(os.environ["FAKE_GCLOUD_LOG"])
args = sys.argv[1:]
with log_path.open("a") as log:
    log.write(json.dumps(args) + "\n")
state = json.loads(state_path.read_text())

def finish(value="", error=None):
    state_path.write_text(json.dumps(state))
    if error is not None:
        print(error, file=sys.stderr)
        sys.exit(1)
    print(value if isinstance(value, str) else json.dumps(value))
    sys.exit(0)

def option(name):
    prefix = name + "="
    for index, arg in enumerate(args):
        if arg.startswith(prefix):
            return arg[len(prefix):]
        if arg == name and index + 1 < len(args):
            return args[index + 1]
    return None

def fail_once(key, message):
    if state.get(key, 0):
        state[key] -= 1
        finish(error=message)

if args == ["config", "get-value", "project"]:
    finish(state["project"])
if args[:2] == ["projects", "describe"]:
    if args[2] != state["project"]:
        finish(error="unexpected project")
    finish("123456789")
if option("--project") != state["project"]:
    finish(error="unexpected or missing project scope")

operation = args[:3]
if args[:2] == ["compute", "instances"]:
    if len(args) < 4 or args[3] != state["name"] or option("--zone") != state["zone"]:
        finish(error="unexpected instance name or zone")
if args[:2] == ["compute", "firewall-rules"]:
    if len(args) < 4 or args[3] != "vcontainer-" + state["name"]:
        finish(error="unexpected firewall name")
if operation == ["compute", "networks", "describe"]:
    if not state.get("network", True):
        finish(error="network not found")
    finish("vcontainer")
if operation == ["compute", "networks", "create"]:
    state["network"] = True
    finish()
if operation == ["compute", "instances", "describe"]:
    if state.get("created") and state.get("replace_after_create"):
        state["instance"] = state.pop("replace_after_create")
    if state.get("instance") is None:
        finish(error="instance not found")
    finish(state["instance"])
if operation == ["compute", "instances", "start"]:
    if state.get("instance") is None:
        finish(error="instance not found")
    if state.get("replace_on_start"):
        state["instance"] = state.pop("replace_on_start")
    else:
        state["instance"]["status"] = "RUNNING"
    finish()
if operation == ["compute", "instances", "create"]:
    if state.get("instance") is not None:
        finish(error="instance already exists")
    state["instance"] = state["create_result"]
    state["created"] = True
    finish([state["instance"]])
if operation == ["compute", "instances", "delete"]:
    state["instance"] = None
    finish()
if operation == ["compute", "firewall-rules", "describe"]:
    if state.get("firewall") is None:
        finish(error="firewall not found")
    finish(state["firewall"])
if operation == ["compute", "firewall-rules", "update"]:
    fail_once("fail_firewall_update", "injected firewall update failure")
    if state.get("firewall") is None:
        finish(error="firewall not found")
    for flag, field in [("--description", "description"), ("--allow", "allow"), ("--source-ranges", "sourceRanges")]:
        value = option(flag)
        if value is not None:
            state["firewall"][field] = value
    finish()
if operation == ["compute", "firewall-rules", "create"]:
    fail_once("fail_firewall_create", "injected firewall create failure before HTTP")
    finish(error="unexpected firewall create: test must stop before HTTP")
if operation == ["compute", "firewall-rules", "delete"]:
    fail_once("fail_firewall_delete", "injected firewall delete failure")
    state["firewall"] = None
    finish()
finish(error="unexpected fake-gcloud operation: " + repr(args))
'''


def instance(identifier=42, status="RUNNING", with_ip=True):
    result = {"id": str(identifier), "status": status}
    if with_ip:
        # The unmatched '[' makes Session::url fail parsing before network I/O.
        result["networkInterfaces"] = [{"accessConfigs": [{"natIP": "[invalid"}]}]
    return result


def firewall(description=OWNER_MARKER, network="vcontainer", tags=None):
    return {
        "description": description,
        "network": f"https://www.googleapis.com/compute/v1/projects/{PROJECT}/global/networks/{network}",
        "targetTags": [RULE] if tags is None else tags,
    }


@unittest.skipUnless(BINARY.is_file(), f"built CLI missing: {BINARY}; run cargo build --locked -p vcontainer first")
class CloudLifecycleTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="vcontainer-cloud-lifecycle-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.state = self.root / "state"
        self.sessions = self.state / "sessions"
        self.sessions.mkdir(parents=True)
        self.session_path = self.sessions / f"{NAME}.json"
        key = self.state / "owner.key"
        key.write_bytes(bytes(range(32)))
        key.chmod(0o600)
        self.release = {
            "image": "vcontainer-test-image", "project": PROJECT,
            "pe_authenticode_sha384": "00" * 48,
            "gpt_event_sha384": "00" * 48, "commit": None,
        }
        (self.state / "release.json").write_text(json.dumps(self.release))
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        stub = self.bin_dir / "gcloud"
        stub.write_text(f"#!{sys.executable}\n" + FAKE_GCLOUD)
        stub.chmod(0o700)
        self.cloud_path = self.root / "fake-cloud.json"
        self.log_path = self.root / "gcloud.jsonl"
        self.env = dict(os.environ)
        self.env.update({
            "HOME": str(self.home), "VCONTAINER_HOME": str(self.state),
            "CLOUDSDK_CONFIG": str(self.root / "gcloud-config"),
            "PATH": str(self.bin_dir), "NO_COLOR": "1", "TERM": "dumb",
            "FAKE_GCLOUD_STATE": str(self.cloud_path),
            "FAKE_GCLOUD_LOG": str(self.log_path),
        })
        self.set_cloud()

    def set_cloud(self, vm=None, rule=None, **options):
        data = {"project": PROJECT, "name": NAME, "zone": ZONE, "instance": vm, "firewall": rule, "network": True}
        data.update(options)
        self.cloud_path.write_text(json.dumps(data))
        self.log_path.write_text("")

    def cloud(self):
        return json.loads(self.cloud_path.read_text())

    def calls(self):
        return [json.loads(line) for line in self.log_path.read_text().splitlines()]

    def operations(self):
        return [tuple(call[:3]) for call in self.calls()]

    def mutations(self):
        return [call for call in self.calls() if call[0] == "compute" and call[2] in {"start", "create", "update", "delete"}]

    def save_session(self):
        session = {
            "name": NAME, "project": PROJECT, "zone": ZONE, "ip": "[invalid",
            "source": "192.0.2.1/32", "ports": [],
            "policy": {
                "tee": "tdx", "owner": base64.b64encode(bytes(32)).decode(),
                "release": self.release,
                "instance": {"id": 42, "project_number": 123456789, "zone": ZONE},
                "launch": None, "min_tcb_eval_num": 20, "min_fw_svn": 2,
            },
        }
        self.session_path.write_text(json.dumps(session, indent=2))
        return self.session_path.read_bytes()

    def cli(self, command, succeeds=False, error=None):
        args = [str(BINARY), command, "--session", NAME]
        if command == "run":
            # Explicitly bypass public-IP lookup (which would otherwise use HTTP).
            args += ["--allow-from", "192.0.2.1/32"]
        result = subprocess.run(args, env=self.env, cwd=self.root, text=True, capture_output=True, timeout=10)
        detail = result.stdout + result.stderr
        if succeeds:
            self.assertEqual(result.returncode, 0, detail)
        else:
            self.assertNotEqual(result.returncode, 0, detail)
        if error:
            self.assertIn(error, detail)
        self.assertNotIn("unexpected fake-gcloud operation", detail)
        return result

    def test_unregistered_same_name_is_never_started_or_adopted(self):
        for status in ("RUNNING", "TERMINATED"):
            with self.subTest(status=status):
                self.set_cloud(vm=instance(status=status), rule=firewall())
                self.cli("run", error="not registered")
                self.assertEqual(self.mutations(), [])
                self.assertFalse(self.session_path.exists())
                self.assertEqual(self.operations().count(("compute", "instances", "describe")), 1)

    def test_changed_saved_instance_id_blocks_run_and_rm(self):
        original = self.save_session()
        for command in ("run", "rm"):
            for status in ("RUNNING", "TERMINATED"):
                with self.subTest(command=command, status=status):
                    self.set_cloud(vm=instance(43, status), rule=firewall())
                    self.cli(command, error="this session owns ID 42")
                    self.assertEqual(self.mutations(), [])
                    self.assertEqual(self.session_path.read_bytes(), original)
                    self.assertEqual(self.cloud()["instance"]["id"], "43")

    def test_missing_saved_instance_requires_rm_before_create(self):
        original = self.save_session()
        self.set_cloud(rule=firewall())
        self.cli("run", error="clean its old record")
        self.assertEqual(self.mutations(), [])
        self.assertEqual(self.session_path.read_bytes(), original)
        self.cli("rm", succeeds=True)
        self.assertFalse(self.session_path.exists())
        self.assertIsNone(self.cloud()["firewall"])
        self.assertNotIn(("compute", "instances", "create"), self.operations())

    def test_start_rechecks_id_before_saving_or_changing_firewall(self):
        original = self.save_session()
        self.set_cloud(vm=instance(status="TERMINATED"), rule=firewall(), replace_on_start=instance(43))
        self.cli("run", error="this session owns ID 42")
        self.assertEqual([call[:3] for call in self.mutations()], [["compute", "instances", "start"]])
        self.assertEqual(self.session_path.read_bytes(), original)
        self.assertEqual(self.cloud()["firewall"], firewall())

    def test_rm_checks_all_resource_ownership_before_deleting_vm(self):
        original = self.save_session()
        foreign_rules = [
            firewall(description=f"vcontainer session {NAME}; instance 43"),
            firewall(network="production"),
            firewall(tags=[RULE, "unrelated"]),
            firewall(description="", tags=[]),
        ]
        for rule in foreign_rules:
            with self.subTest(rule=rule):
                self.set_cloud(vm=instance(), rule=rule)
                self.cli("rm", error="not owned")
                self.assertEqual(self.mutations(), [])
                self.assertEqual(self.session_path.read_bytes(), original)
                self.assertEqual(self.cloud()["instance"]["id"], "42")
                self.assertEqual(self.cloud()["firewall"], rule)

    def test_legacy_firewall_migration_makes_failed_cleanup_retryable(self):
        original = self.save_session()
        self.set_cloud(vm=instance(), rule=firewall(description=""), fail_firewall_delete=1)
        self.cli("rm", error="injected firewall delete failure")
        self.assertEqual(self.session_path.read_bytes(), original)
        self.assertIsNone(self.cloud()["instance"])
        self.assertEqual(self.cloud()["firewall"]["description"], OWNER_MARKER)
        self.assertEqual([call[:3] for call in self.mutations()], [
            ["compute", "firewall-rules", "update"],
            ["compute", "instances", "delete"],
            ["compute", "firewall-rules", "delete"],
        ])
        self.cli("rm", succeeds=True)
        self.assertFalse(self.session_path.exists())
        self.assertIsNone(self.cloud()["firewall"])
        self.assertEqual(self.operations().count(("compute", "instances", "delete")), 1)

    def test_failed_legacy_migration_leaves_vm_and_record_intact(self):
        original = self.save_session()
        self.set_cloud(vm=instance(), rule=firewall(description=""), fail_firewall_update=1)
        self.cli("rm", error="injected firewall update failure")
        self.assertEqual(self.session_path.read_bytes(), original)
        self.assertEqual(self.cloud()["instance"]["id"], "42")
        self.assertEqual(self.cloud()["firewall"]["description"], "")
        self.assertNotIn(("compute", "instances", "delete"), self.operations())

    def test_orphaned_unmarked_firewall_is_not_adopted_without_vm(self):
        original = self.save_session()
        rule = firewall(description="")
        self.set_cloud(rule=rule)
        self.cli("rm", error="not owned")
        self.assertEqual(self.mutations(), [])
        self.assertEqual(self.session_path.read_bytes(), original)
        self.assertEqual(self.cloud()["firewall"], rule)

    def test_owned_cleanup_removes_vm_firewall_and_local_record(self):
        self.save_session()
        self.set_cloud(vm=instance(), rule=firewall())
        self.cli("rm", succeeds=True)
        self.assertIsNone(self.cloud()["instance"])
        self.assertIsNone(self.cloud()["firewall"])
        self.assertFalse(self.session_path.exists())
        self.assertEqual([call[:3] for call in self.mutations()], [
            ["compute", "instances", "delete"], ["compute", "firewall-rules", "delete"],
        ])

    def test_rm_reports_unwritable_record_and_allows_retry(self):
        original = self.save_session()
        # The VM/firewall are already gone; only the stale local record remains.
        self.set_cloud()
        self.sessions.chmod(0o555)
        self.addCleanup(self.sessions.chmod, 0o755)
        probe = self.sessions / "permission-probe"
        try:
            probe.write_text("probe")
        except PermissionError:
            pass
        else:
            probe.unlink()
            self.skipTest("this user can write mode-0555 directories (for example root); rerun as an unprivileged UID")
        failed = self.cli("rm", error="remove session record")
        self.assertNotIn("deleted session", failed.stdout + failed.stderr)
        self.assertEqual(self.session_path.read_bytes(), original)
        self.assertEqual(self.mutations(), [])
        # Restoring access must make the same command finish the local cleanup.
        self.sessions.chmod(0o755)
        self.cli("rm", succeeds=True)
        self.assertFalse(self.session_path.exists())
        self.assertEqual(self.mutations(), [])

    def test_create_uses_array_result_without_second_instance_lookup(self):
        self.set_cloud(create_result=instance(), fail_firewall_create=1)
        self.cli("run", error="injected firewall create failure before HTTP")
        saved = json.loads(self.session_path.read_text())
        self.assertEqual(saved["policy"]["instance"]["id"], 42)
        self.assertEqual(self.operations().count(("compute", "instances", "describe")), 1)
        create = [call for call in self.calls() if call[:3] == ["compute", "instances", "create"]]
        self.assertEqual(len(create), 1)
        self.assertIn("--format=json", create[0])
        self.assertEqual(self.cloud()["instance"]["id"], "42")
        # The saved immutable ID remains sufficient to clean up after failure.
        self.cli("rm", succeeds=True)
        self.assertFalse(self.session_path.exists())

    def test_create_without_ip_rolls_back_vm_only(self):
        foreign = firewall(description="unrelated resource")
        self.set_cloud(rule=foreign, create_result=instance(with_ip=False))
        self.cli("run", error="new VM removed")
        self.assertIsNone(self.cloud()["instance"])
        self.assertEqual(self.cloud()["firewall"], foreign)
        self.assertFalse(self.session_path.exists())
        self.assertFalse(any(call[:2] == ["compute", "firewall-rules"] for call in self.calls()))
        self.assertIn(("compute", "instances", "delete"), self.operations())

    def test_create_rollback_refuses_replacement_id(self):
        foreign = firewall(description="unrelated resource")
        self.set_cloud(rule=foreign, create_result=instance(with_ip=False), replace_after_create=instance(43))
        self.cli("run", error="could not be recorded or safely removed")
        self.assertEqual(self.cloud()["instance"]["id"], "43")
        self.assertEqual(self.cloud()["firewall"], foreign)
        self.assertFalse(self.session_path.exists())
        self.assertNotIn(("compute", "instances", "delete"), self.operations())
        self.assertFalse(any(call[:2] == ["compute", "firewall-rules"] for call in self.calls()))

    def test_create_with_state_write_failure_rolls_back_vm_only(self):
        self.sessions.chmod(0o555)
        self.addCleanup(self.sessions.chmod, 0o755)
        probe = self.sessions / "permission-probe"
        try:
            probe.write_text("probe")
        except PermissionError:
            pass
        else:
            probe.unlink()
            self.skipTest("this user can write mode-0555 directories (for example root); rerun as an unprivileged UID")
        foreign = firewall(description="unrelated resource")
        self.set_cloud(rule=foreign, create_result=instance())
        self.cli("run", error="new VM removed")
        self.assertIsNone(self.cloud()["instance"])
        self.assertEqual(self.cloud()["firewall"], foreign)
        self.assertFalse(self.session_path.exists())
        self.assertEqual(list(self.sessions.iterdir()), [])
        self.assertFalse(any(call[:2] == ["compute", "firewall-rules"] for call in self.calls()))


if __name__ == "__main__":
    unittest.main()
