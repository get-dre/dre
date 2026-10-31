#!/usr/bin/env python3
"""Package seam: deterministic archives and user-managed Java selection/cleanup."""

import csv
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest
from zipfile import ZipFile

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("package_jdbc", ROOT / ".github/scripts/package_jdbc.py")
package_jdbc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_jdbc)
SUFFIX = ".exe" if os.name == "nt" else ""


def launcher(base):
    destination = base / f"dre-plugin-jdbc{SUFFIX}"
    source = ROOT / "java/jdbc/launcher/dre-plugin-jdbc"
    if os.name == "nt":
        subprocess.run(["rustc", "--edition=2021", str(source.with_suffix(".rs")),
                        "-o", str(destination)], check=True)
    else:
        shutil.copy2(source, destination)
    (base / "dre-plugin-jdbc.jar").write_bytes(b"jar")
    return destination


def fake_java(home):
    executable = home / f"bin/java{SUFFIX}"
    executable.parent.mkdir(parents=True)
    if os.name == "nt":
        subprocess.run(["rustc", "--edition=2021", str(ROOT / "java/jdbc/launcher/fake_java.rs"),
                        "-o", str(executable)], check=True)
    else:
        executable.write_text(
            '#!/bin/sh\nif [ "$1" = -version ]; then\n'
            '  printf \'Picked up JAVA_TOOL_OPTIONS: -Dexample="quoted value"\\n\' >&2\n'
            '  printf \'openjdk version "%s"\\n\' "${DRE_FAKE_VERSION:-17.0.1}" >&2\n'
            '  exit 0\nfi\nprintf \'%s\\n\' "$@" > "$DRE_FAKE_ARGS"\n')
        executable.chmod(0o755)
    return executable


class Package(unittest.TestCase):
    def test_shaded_jar_keeps_overlapping_dependency_notices(self):
        jar = ROOT / "java/jdbc/target/dre-plugin-jdbc.jar"
        if not jar.is_file():
            self.skipTest("build the JDBC JAR first")
        with ZipFile(jar) as archive:
            self.assertIn(b"Copyright (c) 2004-2023 QOS.ch", archive.read("META-INF/LICENSE.txt"))
            notice = archive.read("META-INF/NOTICE")
            for name in (b"Arrow JDBC Adapter", b"Arrow Memory - Core", b"Jackson JSON processor"):
                self.assertIn(name, notice)

    def test_archive_is_reproducible_and_contains_no_java_or_driver(self):
        with tempfile.TemporaryDirectory(prefix="dre-jdbc-package-") as temporary:
            base = Path(temporary)
            jar = base / "source.jar"
            jar.write_bytes(b"jar")
            outputs = []
            for index in range(2):
                output = base / f"jdbc-{index}.tar.gz"
                package_jdbc.package(
                    output, platform="macos-aarch64", jar=jar,
                    launcher=ROOT / "java/jdbc/launcher/dre-plugin-jdbc",
                    license=ROOT / "LICENSE", notice=ROOT / "NOTICE",
                    third_party=ROOT / "THIRD_PARTY_NOTICES.md")
                outputs.append(hashlib.sha256(output.read_bytes()).hexdigest())
            self.assertEqual(outputs[0], outputs[1])
            with tarfile.open(base / "jdbc-0.tar.gz") as archive:
                self.assertEqual(set(archive.getnames()), {
                    "jdbc/dre-plugin-jdbc", "jdbc/dre-plugin-jdbc.jar",
                    "jdbc/LICENSE", "jdbc/NOTICE", "jdbc/THIRD_PARTY_NOTICES.md"})
                self.assertTrue(all(member.isfile() for member in archive.getmembers()))

    def test_launcher_accepts_compatible_java_home_and_path_with_spaces(self):
        with tempfile.TemporaryDirectory(prefix="dre jdbc launch ") as temporary:
            base = Path(temporary)
            executable = launcher(base)
            java = fake_java(base / "user Java")
            observed = base / "arguments.txt"
            for mode in ("JAVA_HOME", "PATH"):
                for version in ("17.0.1", "21.0.9", "25.0.1", "99-ea"):
                    with self.subTest(mode=mode, version=version):
                        env = os.environ | {
                            "JAVA_HOME": str(java.parent.parent) if mode == "JAVA_HOME" else "",
                            "PATH": str(base) if mode == "JAVA_HOME" else str(java.parent),
                            "DRE_FAKE_ARGS": str(observed), "DRE_FAKE_VERSION": version}
                        run = subprocess.run([executable, "protocol", "argument"], env=env,
                                             capture_output=True, timeout=10)
                        self.assertEqual(run.returncode, 0, run.stderr)
                        self.assertEqual(run.stdout, b"")
                        self.assertEqual(observed.read_text().splitlines(), [
                            "--add-opens=java.base/java.nio=ALL-UNNAMED", "-jar",
                            str(base / "dre-plugin-jdbc.jar"), "protocol", "argument"])

    def test_missing_or_old_java_fails_with_setup_guidance(self):
        with tempfile.TemporaryDirectory(prefix="dre jdbc missing ") as temporary:
            base = Path(temporary)
            executable = launcher(base)
            env = os.environ | {"JAVA_HOME": str(base / "absent"), "PATH": str(base)}
            for version in (None, "1.8.0_471", "11.0.29", "16", "unknown"):
                with self.subTest(version=version):
                    if version is not None:
                        if "DRE_FAKE_VERSION" not in env:
                            java = fake_java(base / "old Java")
                            env["JAVA_HOME"] = str(java.parent.parent)
                        env["DRE_FAKE_VERSION"] = version
                    run = subprocess.run([executable], env=env, capture_output=True, timeout=10)
                    self.assertNotEqual(run.returncode, 0)
                    self.assertEqual(run.stdout, b"")
                    self.assertIn(b"Java 17 or newer", run.stderr)
                    self.assertIn(b"JAVA_HOME", run.stderr)

    @unittest.skipUnless(os.name == "nt", "the native launcher is Windows-only")
    def test_forced_launcher_termination_also_stops_java(self):
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
        with tempfile.TemporaryDirectory(prefix="dre jdbc cleanup ") as temporary:
            base = Path(temporary)
            executable = launcher(base)
            java = fake_java(base / "user Java")
            pid = base / "runtime.pid"
            env = os.environ | {"JAVA_HOME": str(java.parent.parent), "PATH": str(base),
                                "DRE_FAKE_ARGS": str(base / "arguments"), "DRE_FAKE_PID": str(pid)}
            process = subprocess.Popen([executable], env=env)
            handle = None
            try:
                deadline = time.monotonic() + 10
                while not pid.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(pid.exists(), "fake runtime did not start")
                handle = kernel.OpenProcess(0x00100001, False, int(pid.read_text()))
                self.assertTrue(handle, "can't watch the fake runtime")
                process.kill()
                process.wait(timeout=10)
                self.assertEqual(kernel.WaitForSingleObject(handle, 10000), 0,
                                 "Java survived forced launcher termination")
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=10)
                if handle:
                    kernel.TerminateProcess(handle, 1)
                    kernel.CloseHandle(handle)

    def test_release_archive_launches_with_installed_java(self):
        artifact = os.environ.get("DRE_TEST_JDBC_ARCHIVE")
        if not artifact:
            self.skipTest("set DRE_TEST_JDBC_ARCHIVE to the prepared release archive")
        with tempfile.TemporaryDirectory(prefix="dre jdbc real archive ") as temporary:
            base = Path(temporary)
            with tarfile.open(artifact) as archive:
                archive.extractall(base, filter="data")
            package = base / "jdbc"
            self.assertFalse((package / "runtime").exists())
            process = subprocess.Popen([package / f"dre-plugin-jdbc{SUFFIX}"], stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            replies = queue.Queue()

            def read_hello():
                header = process.stdout.read(4)
                replies.put(process.stdout.read(int.from_bytes(header, "big")) if header else b"")

            threading.Thread(target=read_hello, daemon=True).start()
            try:
                body = b'J{"type":"hello","min_version":1,"max_version":1,"plugin":"source/jdbc"}'
                process.stdin.write(len(body).to_bytes(4, "big") + body)
                process.stdin.flush()
                reply = replies.get(timeout=30)
                self.assertEqual(reply[:1], b"J", reply)
                self.assertEqual(json.loads(reply[1:])["type"], "hello")
                process.stdin.close()
                self.assertEqual(process.wait(timeout=10), 0)
                self.assertEqual(process.stderr.read(), b"")
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=10)
                for stream in (process.stdin, process.stdout, process.stderr):
                    stream.close()

    def test_install_update_and_report_with_installed_java(self):
        artifact = os.environ.get("DRE_TEST_JDBC_ARCHIVE")
        cli = os.environ.get("DRE_BIN")
        driver = os.environ.get("DRE_TEST_H2_JAR")
        csv_plugin = os.environ.get("DRE_TEST_CSV_BIN")
        if not all((artifact, cli, driver, csv_plugin)):
            self.skipTest("set archive, DRE_BIN, DRE_TEST_H2_JAR and DRE_TEST_CSV_BIN for install/report")
        artifact = Path(artifact).resolve()
        with tempfile.TemporaryDirectory(prefix="dre jdbc installed report ") as temporary:
            base = Path(temporary)
            project = base / "project"
            project.mkdir()
            (project / "dre_project.yml").write_text("name: packaged\ndefault_profile: fixture\n")
            (project / "dependencies.yml").write_text("plugins: [jdbc, csv]\n")
            (project / "profiles.yml").write_text(
                "connections:\n  fixture:\n    targets:\n      dev:\n        type: jdbc\n"
                "        url: 'jdbc:h2:mem:packaged'\n"
                f"        driver_jars: {json.dumps(str(Path(driver).resolve()))}\n")
            reports = project / "reports/check"
            reports.mkdir(parents=True)
            (reports / "check.yml").write_text(
                "queries:\n  - {query: prepare, tab: false}\n  - rows\noutput: {format: csv}\n")
            (reports / "prepare.sql").write_text("set @report_value = 42;\n")
            (reports / "rows.sql").write_text("select @report_value as \"value\";\n")
            platform = os.environ["DRE_TEST_PACKAGE_PLATFORM"]
            index = base / "registry.json"
            index.write_text(json.dumps({"schema": 2, "plugins": [{
                "name": "jdbc", "provides": ["source/jdbc"], "versions": [{
                    "version": "1.0.0", "protocol": 1, "artifacts": {platform: {
                        "url": str(artifact), "sha256": hashlib.sha256(artifact.read_bytes()).hexdigest()}}}]}]}))
            plugins = base / "plugins with spaces"
            plugins.mkdir()
            shutil.copy2(csv_plugin, plugins / Path(csv_plugin).name)
            env = os.environ | {"DRE_REGISTRY_URL": str(index), "DRE_PLUGINS_DIR": str(plugins),
                                "HOME": str(base / "home"), "USERPROFILE": str(base / "home")}

            def run(*args):
                command = [str(Path(cli).resolve()), *args]
                if args[0] == "run":
                    command.extend(["--profiles-dir", str(project)])
                result = subprocess.run(command, cwd=project, env=env,
                                        capture_output=True, text=True, timeout=90)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            run("plugin", "install", "jdbc")
            lock = (project / "dre.lock").read_bytes()
            run("plugin", "update", "jdbc")
            self.assertEqual((project / "dre.lock").read_bytes(), lock)
            run("run", "check", "--preview")
            output, = project.glob("target/run/**/check.csv")
            with output.open(newline="") as stream:
                self.assertEqual(list(csv.reader(stream)), [["value"], ["42"]])


if __name__ == "__main__":
    unittest.main()
