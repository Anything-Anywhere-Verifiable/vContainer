"""Installer contract checks, without compiling or accessing the cloud.

Run: PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p 'test_*.py'
"""

from contextlib import redirect_stderr, redirect_stdout
import io
import os
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

import install


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.output = io.StringIO()
        for redirect in (redirect_stdout(self.output), redirect_stderr(self.output)):
            redirect.__enter__()
            self.addCleanup(redirect.__exit__, None, None, None)

    def test_dry_run_does_not_check_or_execute_anything(self):
        with patch.object(install, "preflight") as preflight, patch.object(install.subprocess, "run") as execute:
            self.assertEqual(install.main(["--publish", "--project", "example", "--dry-run"]), 0)
        preflight.assert_not_called()
        execute.assert_not_called()
        self.assertIn("publish --dir", self.output.getvalue())
        self.assertIn("Preview only", self.output.getvalue())

    def test_local_setup_runs_from_repo_and_does_not_publish(self):
        with patch.object(install, "preflight"), patch.object(install, "ensure_rust"), patch.object(install.subprocess, "run") as execute:
            self.assertEqual(install.main(["--dev", "--clean", "--install-root", "/tmp/cli root"]), 0)
        commands = [call.args[0] for call in execute.call_args_list]
        self.assertEqual(commands[0], ["bash", str(install.REPO / "scripts/build-image.sh"), "--dev", "--clean"])
        self.assertEqual(commands[1][-2:], ["--root", "/tmp/cli root"])
        self.assertEqual(commands[-1], ["/tmp/cli root/bin/vcontainer", "measure", "--dir", str(install.REPO / "out")])
        self.assertEqual(len(commands), 4)
        for call in execute.call_args_list:
            self.assertEqual(call.kwargs["cwd"], install.REPO)
            self.assertTrue(call.kwargs["check"])

    def test_publish_uses_requested_project_without_changing_parent_environment(self):
        with patch.dict(os.environ, {"CLOUDSDK_CORE_PROJECT": "existing"}):
            with patch.object(install, "preflight"), patch.object(install, "ensure_rust"), patch.object(install.subprocess, "run") as execute:
                self.assertEqual(install.main(["--publish", "--project", "requested"]), 0)
            self.assertEqual(os.environ["CLOUDSDK_CORE_PROJECT"], "existing")
        self.assertEqual(execute.call_args.args[0][1], "publish")
        for call in execute.call_args_list:
            self.assertEqual(call.kwargs["env"]["CLOUDSDK_CORE_PROJECT"], "requested")

    def test_failed_build_stops_before_install_and_publish(self):
        with patch.object(install, "preflight"), patch.object(install, "ensure_rust"), patch.object(
            install.subprocess, "run", side_effect=subprocess.CalledProcessError(42, ["bash"]),
        ) as execute:
            self.assertEqual(install.main(["--publish", "--project", "example"]), 42)
        self.assertEqual(execute.call_count, 1)
        self.assertNotIn("CLI installed:", self.output.getvalue())

    def test_cli_only_installs_and_checks_cli_without_boot_artifacts(self):
        with patch.object(install, "preflight"), patch.object(install, "ensure_rust"), patch.object(install.subprocess, "run") as execute:
            self.assertEqual(install.main(["--cli-only"]), 0)
        commands = [call.args[0] for call in execute.call_args_list]
        self.assertEqual(len(commands), 2)
        self.assertEqual(commands[0][:3], ["cargo", "install", "--locked"])
        self.assertEqual(commands[1][1:], ["--help"])

    def test_invalid_options_exit_before_execution(self):
        for argv in (["--publish"], ["--project", "example"], ["--cli-only", "--dev"],
                     ["--cli-only", "--clean"], ["--cli-only", "--publish", "--project", "example"]):
            with self.subTest(argv=argv), patch.object(install.subprocess, "run") as execute:
                with self.assertRaises(SystemExit) as error:
                    install.main(argv)
                self.assertEqual(error.exception.code, 2)
                execute.assert_not_called()

    def test_macos_cli_only_needs_no_docker(self):
        with patch.object(install.platform, "system", return_value="Darwin"), patch.object(
            install.shutil, "which", side_effect=lambda name: "/bin/cargo" if name == "cargo" else None,
        ), patch.object(install.subprocess, "run") as execute:
            install.preflight(install.arguments(["--cli-only"]), {})
        execute.assert_not_called()

    def test_full_build_on_macos_fails_before_running_tools(self):
        with patch.object(install.platform, "system", return_value="Darwin"), patch.object(
            install.subprocess, "run",
        ) as execute:
            self.assertEqual(install.main([]), 1)
        execute.assert_not_called()
        self.assertIn("--cli-only", self.output.getvalue())

    def test_dirty_release_fails_before_docker_or_build(self):
        with patch.object(install.platform, "system", return_value="Linux"), patch.object(
            install.platform, "machine", return_value="x86_64",
        ), patch.object(install.shutil, "which", return_value="/bin/tool"), patch.object(
            install.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, " M README.md\n"),
        ) as execute:
            self.assertEqual(install.main([]), 1)
        self.assertEqual(execute.call_count, 1)
        self.assertIn("clean checkout", self.output.getvalue())

    def test_missing_bootstrap_dependency_is_actionable(self):
        with patch.object(install.platform, "system", return_value="Linux"), patch.object(
            install.shutil, "which", return_value=None,
        ), patch.object(install.subprocess, "run") as execute:
            self.assertEqual(install.main(["--cli-only"]), 1)
        execute.assert_not_called()
        self.assertIn("Missing curl", self.output.getvalue())

    def test_existing_rustup_prepares_toolchain_without_bootstrap(self):
        env = {"CARGO_HOME": "/tmp/rust tools", "PATH": "/usr/bin"}
        with patch.object(install.shutil, "which", return_value="/tmp/rust tools/bin/rustup"), patch.object(
            install.subprocess, "run",
        ) as execute:
            install.ensure_rust(env)
        commands = [call.args[0] for call in execute.call_args_list]
        self.assertEqual(commands, [
            ["/tmp/rust tools/bin/rustup", "toolchain", "install", "--profile", "minimal", "--no-self-update"],
            ["cargo", "--version"],
        ])
        self.assertEqual(env["PATH"], "/tmp/rust tools/bin" + os.pathsep + "/usr/bin")
        self.assertEqual(execute.call_args.kwargs["cwd"], install.REPO)

    def test_missing_rustup_bootstraps_then_prepares_toolchain(self):
        env = {"CARGO_HOME": "/tmp/rust tools", "PATH": "/usr/bin"}
        def which(name, **kwargs):
            return None if name == "rustup" else f"/usr/bin/{name}"
        with patch.object(install.shutil, "which", side_effect=which), patch.object(
            install.subprocess, "run",
        ) as execute:
            install.ensure_rust(env)
        commands = [call.args[0] for call in execute.call_args_list]
        self.assertEqual(commands[0][0], "curl")
        self.assertIn("https://sh.rustup.rs", commands[0])
        script = commands[0][-1]
        self.assertEqual(commands[1], [
            "sh", script, "-y", "--no-modify-path", "--profile", "minimal", "--default-toolchain", "none",
        ])
        self.assertEqual(commands[2][:3], ["/tmp/rust tools/bin/rustup", "toolchain", "install"])
        self.assertEqual(commands[3], ["cargo", "--version"])
        self.assertFalse(Path(script).exists())

    def test_missing_rustup_dry_run_does_not_download_or_create_files(self):
        with patch.object(install.shutil, "which", return_value=None), patch.object(
            install.subprocess, "run",
        ) as execute, patch.object(install.tempfile, "TemporaryDirectory") as temporary:
            self.assertEqual(install.main(["--cli-only", "--dry-run"]), 0)
        execute.assert_not_called()
        temporary.assert_not_called()
        self.assertIn("https://sh.rustup.rs", self.output.getvalue())
        self.assertIn("toolchain install", self.output.getvalue())

    def test_failed_rust_setup_stops_before_build(self):
        with patch.object(install, "preflight"), patch.object(install.shutil, "which", return_value="/bin/rustup"), patch.object(
            install.subprocess, "run", side_effect=subprocess.CalledProcessError(1, ["rustup"]),
        ) as execute:
            self.assertEqual(install.main([]), 1)
        self.assertEqual(execute.call_count, 1)
        self.assertNotIn("build-image.sh", self.output.getvalue())

    def test_publish_auth_check_never_prints_token(self):
        with patch.object(install.platform, "system", return_value="Linux"), patch.object(
            install.platform, "machine", return_value="x86_64",
        ), patch.object(install.shutil, "which", return_value="/bin/tool"), patch.object(
            install.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, ""),
        ) as execute:
            install.preflight(install.arguments(["--publish", "--project", "example"]), {})
        self.assertEqual(execute.call_args.args[0], ["gcloud", "auth", "print-access-token"])
        self.assertEqual(execute.call_args.kwargs["stdout"], subprocess.DEVNULL)


if __name__ == "__main__":
    unittest.main()
