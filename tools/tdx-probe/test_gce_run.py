"""Exercise the real probe runner against a local fake gcloud; no cloud access.

Run: PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/tdx-probe -p 'test_*.py'
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


RUNNER = Path(__file__).with_name("gce-run.sh")
FAKE_GCLOUD = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
state_path = pathlib.Path(os.environ["FAKE_STATE"])
state = json.loads(state_path.read_text())
state["calls"].append(args)
scenario = os.environ.get("FAKE_SCENARIO", "pass")

def finish(code=0, output=""):
    state_path.write_text(json.dumps(state))
    if output:
        print(output)
    sys.exit(code)

def option(name):
    for i, a in enumerate(args):
        if a == name:
            return args[i + 1]
        if a.startswith(name + "="):
            return a.split("=", 1)[1]
    return ""

if "compute" not in args:
    finish()
i = args.index("compute")
kind, action = args[i + 1:i + 3]
name = args[i + 3] if len(args) > i + 3 else ""
key = kind + "/" + name
if kind == "networks":
    finish()
if action == "create":
    run_id = dict(x.split("=", 1) for x in option("--labels").split(","))["vcontainer_run"]
    if scenario == "image_collision" and kind == "images" or scenario in ("vm_collision", "vm_collision_cleanup_failure") and kind == "instances":
        state["resources"][key] = {"id": "900", "run": "someone_else"}
        finish(42)
    resource_id = "101" if kind == "images" else "201"
    state["resources"][key] = {"id": resource_id, "run": run_id}
    finish(output=resource_id)
if action == "describe":
    r = state["resources"].get(key)
    if not r:
        finish(1)
    if kind == "instances" and scenario == "changed_id":
        r = dict(r, id="999")
    if kind == "instances" and scenario == "changed_label":
        r = dict(r, run="someone_else")
    finish(output=r["id"] + "\t" + r["run"])
if action == "delete":
    if kind == "instances" and scenario == "delete_failure" or kind == "images" and scenario == "vm_collision_cleanup_failure":
        finish(1)
    state["resources"].pop(key, None)
    finish()
if action == "get-serial-port-output":
    output = {
        "panic": "Kernel panic: fake\n",
        "timeout": "starting\n",
        "check_failure": "ERR quote: rejected\nPROBE RESULT FAIL\nPROBE DONE\n",
        "done_only": "PROBE DONE\n",
        "pass_with_error": "ERR quote: rejected\nPROBE RESULT PASS\nPROBE DONE\n",
    }.get(scenario, "PROBE RESULT PASS\nPROBE DONE\n")
    finish(output=output)
finish(1)
'''


class ProbeRunnerTests(unittest.TestCase):
    def run_scenario(self, scenario="pass", shapes=()):
        with tempfile.TemporaryDirectory(prefix="vcontainer-probe-test-") as td:
            repo = Path(td)
            scripts = repo / "tools/tdx-probe"
            scripts.mkdir(parents=True)
            shutil.copyfile(RUNNER, scripts / "gce-run.sh")
            (repo / "out").mkdir()
            (repo / "out/vcontainer.tar.gz").write_bytes(b"not a cloud image")
            fake_bin = repo / "fake-bin"
            fake_bin.mkdir()
            fake = fake_bin / "gcloud"
            fake.write_text(FAKE_GCLOUD)
            fake.chmod(0o755)
            state_path = repo / "state.json"
            state_path.write_text(json.dumps({"calls": [], "resources": {}}))
            env = dict(os.environ, PATH=str(fake_bin) + os.pathsep + os.environ["PATH"],
                       PROJECT="test-project", BUCKET="test-bucket", FAKE_STATE=str(state_path),
                       FAKE_SCENARIO=scenario, PROBE_POLL_TIMEOUT="0", PROBE_POLL_INTERVAL="0")
            result = subprocess.run(["bash", str(scripts / "gce-run.sh"), *shapes],
                                    env=env, text=True, capture_output=True, timeout=10)
            state = json.loads(state_path.read_text())
            return result, state

    @staticmethod
    def deletes(state, kind):
        return [c for c in state["calls"] if kind in c and "delete" in c]

    def test_success_cleans_created_vm_and_image(self):
        result, state = self.run_scenario()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(state["resources"], {})
        self.assertEqual(len(self.deletes(state, "instances")), 1)
        self.assertEqual(len(self.deletes(state, "images")), 1)
        self.assertIn("PROBE RESULT PASS", result.stdout)

    def test_image_collision_is_never_deleted(self):
        result, state = self.run_scenario("image_collision")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.deletes(state, "images"), [])
        self.assertEqual(next(iter(state["resources"].values()))["run"], "someone_else")

    def test_vm_collision_is_never_deleted(self):
        result, state = self.run_scenario("vm_collision")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.deletes(state, "instances"), [])
        self.assertEqual(len(self.deletes(state, "images")), 1)
        self.assertEqual(next(iter(state["resources"].values()))["run"], "someone_else")

    def test_changed_id_or_label_is_not_deleted(self):
        for scenario in ("changed_id", "changed_label"):
            with self.subTest(scenario=scenario):
                result, state = self.run_scenario(scenario)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.deletes(state, "instances"), [])
                self.assertIn("ownership changed", result.stderr)

    def test_probe_failures_return_nonzero_and_clean_up(self):
        for scenario in ("panic", "timeout", "check_failure", "done_only", "pass_with_error"):
            with self.subTest(scenario=scenario):
                result, state = self.run_scenario(scenario)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertEqual(state["resources"], {})
                self.assertEqual(len(self.deletes(state, "instances")), 1)
                self.assertEqual(len(self.deletes(state, "images")), 1)

    def test_cleanup_failure_cannot_return_success(self):
        result, state = self.run_scenario("delete_failure")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not delete instances", result.stderr)
        self.assertEqual(len(self.deletes(state, "images")), 1)

    def test_cleanup_preserves_an_original_creation_failure(self):
        result, state = self.run_scenario("vm_collision_cleanup_failure")
        self.assertEqual(result.returncode, 42)
        self.assertEqual(self.deletes(state, "instances"), [])
        self.assertIn("could not delete images", result.stderr)

    def test_duplicate_shapes_get_distinct_names(self):
        result, state = self.run_scenario(shapes=("c3-standard-4", "c3-standard-4"))
        self.assertEqual(result.returncode, 0, result.stderr)
        names = [c[c.index("create") + 1] for c in state["calls"] if "instances" in c and "create" in c]
        self.assertEqual(len(set(names)), 2)
        self.assertTrue(all(len(n) <= 63 for n in names))
        self.assertEqual(state["resources"], {})


if __name__ == "__main__":
    unittest.main()
