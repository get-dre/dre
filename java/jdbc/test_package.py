#!/usr/bin/env python3
"""Package seam: deterministic archives and a launcher independent of system Java."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PACKAGER = ROOT / ".github/scripts/package_jdbc.py"
spec = importlib.util.spec_from_file_location("package_jdbc", PACKAGER)
package_jdbc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_jdbc)


class Package(unittest.TestCase):
    def test_archive_is_complete_version_directory_and_reproducible(self):
        with tempfile.TemporaryDirectory(prefix="dre-jdbc-package-") as temporary:
            base = Path(temporary)
            jar = base / "dre-plugin-jdbc.jar"
            jar.write_bytes(b"jar")
            runtime = base / "runtime"
            (runtime / "bin").mkdir(parents=True)
            runtime_java = runtime / "bin/java"
            runtime_java.write_text("#!/bin/sh\nexit 0\n")
            runtime_java.chmod(0o755)
            (runtime / "legal/java.base").mkdir(parents=True)
            (runtime / "legal/java.base/LICENSE").write_text("runtime license\n")
            (runtime / "LICENSE").write_text("runtime terms\n")
            (runtime / "NOTICE").write_text("runtime notice\n")
            (runtime / "release").write_text('JAVA_VERSION="17.0.20.1"\n')
            outputs = []
            for index in range(2):
                output = base / f"jdbc-{index}.tar.gz"
                package_jdbc.package(
                    output, version="1.0.0", platform="macos-aarch64", jar=jar,
                    runtime=runtime, launcher=ROOT / "java/jdbc/launcher/dre-plugin-jdbc",
                    license=ROOT / "LICENSE", notice=ROOT / "NOTICE",
                    third_party=ROOT / "THIRD_PARTY_NOTICES.md",
                )
                outputs.append(hashlib.sha256(output.read_bytes()).hexdigest())
            self.assertEqual(outputs[0], outputs[1])
            with tarfile.open(base / "jdbc-0.tar.gz") as archive:
                names = set(archive.getnames())
                expected = {
                    "jdbc/dre-plugin-jdbc", "jdbc/dre-plugin-jdbc.jar",
                    "jdbc/runtime/bin/java", "jdbc/runtime/legal/java.base/LICENSE",
                    "jdbc/runtime/LICENSE", "jdbc/runtime/NOTICE",
                    "jdbc/LICENSE", "jdbc/NOTICE", "jdbc/THIRD_PARTY_NOTICES.md",
                }
                self.assertLessEqual(expected, names)
                self.assertFalse(any(member.issym() or member.islnk() for member in archive.getmembers()))

    def test_runtime_legal_links_become_regular_files(self):
        with tempfile.TemporaryDirectory(prefix="dre-jdbc-legal-") as temporary:
            base = Path(temporary)
            runtime = base / "runtime"
            (runtime / "legal/java.base").mkdir(parents=True)
            (runtime / "legal/java.base/LICENSE").write_bytes(b"license\n")
            (runtime / "legal/java.sql").mkdir()
            (runtime / "legal/java.sql/LICENSE").symlink_to("../java.base/LICENSE")
            (runtime / "bin").mkdir()
            (runtime / "bin/java").write_text("#!/bin/sh\nexit 0\n")
            (runtime / "bin/java").chmod(0o755)
            (runtime / "NOTICE").write_text("notice\n")
            jar = base / "dre-plugin-jdbc.jar"
            jar.write_bytes(b"jar")
            output = base / "jdbc.tar.gz"
            package_jdbc.package(
                output, version="1.0.0", platform="linux-x86_64", jar=jar, runtime=runtime,
                launcher=ROOT / "java/jdbc/launcher/dre-plugin-jdbc",
                license=ROOT / "LICENSE", notice=ROOT / "NOTICE",
                third_party=ROOT / "THIRD_PARTY_NOTICES.md",
            )
            with tarfile.open(output) as archive:
                info = archive.getmember("jdbc/runtime/legal/java.sql/LICENSE")
                self.assertTrue(info.isfile())
                self.assertEqual(archive.extractfile(info).read(), b"license\n")

    @unittest.skipIf(os.name == "nt", "the packaged POSIX launcher is not used on Windows")
    def test_packaged_launcher_ignores_java_home_and_path(self):
        with tempfile.TemporaryDirectory(prefix="dre jdbc launch ") as temporary:
            base = Path(temporary)
            package = base / "jdbc/1.0.0"
            (package / "runtime/bin").mkdir(parents=True)
            observed = base / "arguments.txt"
            (package / "runtime/bin/java").write_text(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$DRE_FAKE_ARGS\"\n"
            )
            (package / "runtime/bin/java").chmod(0o755)
            (package / "dre-plugin-jdbc.jar").write_bytes(b"jar")
            external = base / "external"
            external.mkdir()
            external_java = external / "java"
            external_java.write_text("#!/bin/sh\nexit 97\n")
            external_java.chmod(0o755)
            env = os.environ | {
                "JAVA_HOME": str(base / "external"),
                "PATH": f"{external}{os.pathsep}{os.environ['PATH']}",
                "DRE_FAKE_ARGS": str(observed),
            }
            launcher = package / "dre-plugin-jdbc"
            launcher.write_bytes((ROOT / "java/jdbc/launcher/dre-plugin-jdbc").read_bytes())
            launcher.chmod(0o755)
            run = subprocess.run([launcher, "protocol", "argument"], env=env, check=False)
            self.assertEqual(run.returncode, 0)
            self.assertEqual(observed.read_text().splitlines(), [
                "--add-opens=java.base/java.nio=ALL-UNNAMED",
                "-jar", str(package / "dre-plugin-jdbc.jar"), "protocol", "argument",
            ])

    def test_jlink_runtime_starts_the_built_jar(self):
        java_home = os.environ.get("JAVA_HOME")
        jar = ROOT / "java/jdbc/target/dre-plugin-jdbc.jar"
        if not java_home or not (Path(java_home) / "bin/jlink").is_file() or not jar.is_file():
            self.skipTest("set JAVA_HOME to JDK 17 and build the JDBC JAR first")
        # Leave the runtime beside the JAR for the describe.json check that follows in CI.
        runtime = jar.parent / "runtime"
        if runtime.exists():
            shutil.rmtree(runtime)
        modules = "java.base,java.desktop,java.sql,jdk.jfr,jdk.unsupported"
        subprocess.run([
            str(Path(java_home) / "bin/jlink"), f"--add-modules={modules}",
            "--strip-debug", "--no-header-files", "--no-man-pages", f"--compress=2",
            "--output", str(runtime),
        ], check=True)
        launch = subprocess.Popen(
            [str(runtime / "bin/java"), "--add-opens=java.base/java.nio=ALL-UNNAMED", "-jar", str(jar)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
        )
        body = b"J" + b'{"type":"hello","min_version":0,"max_version":1,"plugin":"source/jdbc"}'
        launch.stdin.write(len(body).to_bytes(4, "big") + body)
        launch.stdin.flush()
        header = launch.stdout.read(4)
        if len(header) != 4:
            status = launch.wait(timeout=10)
            diagnostic = launch.stderr.read().decode(errors="replace")
            self.fail(f"bundled runtime exited before replying: {status}: {diagnostic}")
        length = int.from_bytes(header, "big")
        reply = launch.stdout.read(length)
        self.assertEqual(reply[:1], b"J")
        self.assertEqual(json.loads(reply[1:])["type"], "hello")
        launch.stdin.close()
        self.assertEqual(launch.wait(timeout=10), 0, launch.stderr.read().decode())

    @unittest.skipUnless(os.name == "nt", "the native launcher is Windows-only")
    def test_windows_launcher_uses_adjacent_java_exe_only(self):
        with tempfile.TemporaryDirectory(prefix="dre jdbc windows ") as temporary:
            base = Path(temporary)
            launcher = base / "dre-plugin-jdbc.exe"
            subprocess.run([
                "rustc", "--edition=2021", str(ROOT / "java/jdbc/launcher/dre-plugin-jdbc.rs"),
                "-o", str(launcher),
            ], check=True)
            runtime = base / "runtime/bin"
            runtime.mkdir(parents=True)
            subprocess.run([
                "rustc", "--edition=2021", str(ROOT / "java/jdbc/launcher/fake_java.rs"),
                "-o", str(runtime / "java.exe"),
            ], check=True)
            (base / "dre-plugin-jdbc.jar").write_bytes(b"jar")
            external = base / "external"
            external.mkdir()
            (external / "java.exe").write_bytes(b"must not run")
            observed = base / "arguments.txt"
            env = os.environ | {
                "JAVA_HOME": str(external),
                "PATH": str(external),
                "DRE_FAKE_ARGS": str(observed),
            }
            run = subprocess.run([launcher, "protocol", "argument"], env=env, check=False)
            self.assertEqual(run.returncode, 0)
            self.assertEqual(observed.read_text().splitlines(), [
                "--add-opens=java.base/java.nio=ALL-UNNAMED",
                "-jar", str(base / "dre-plugin-jdbc.jar"), "protocol", "argument",
            ])


if __name__ == "__main__":
    unittest.main()
