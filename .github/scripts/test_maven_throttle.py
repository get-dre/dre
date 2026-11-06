#!/usr/bin/env python3
"""Exercise JDBC's real Maven model against a throttled, checksum-verified local repository."""

import hashlib
import http.server
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
POM = ROOT / "java/jdbc/pom.xml"
NS = {"m": "http://maven.apache.org/POM/4.0.0"}
IMPORT = ET.parse(POM).find("m:dependencyManagement/m:dependencies/m:dependency", NS)
GROUP, ARTIFACT, VERSION = [IMPORT.findtext("m:" + key, namespaces=NS)
                            for key in ("groupId", "artifactId", "version")]
ARTIFACT_PATH = f"/{GROUP.replace('.', '/')}/{ARTIFACT}/{VERSION}/{ARTIFACT}-{VERSION}.pom"
BOM = f"""<project xmlns="http://maven.apache.org/POM/4.0.0">
<modelVersion>4.0.0</modelVersion><groupId>{GROUP}</groupId>
<artifactId>{ARTIFACT}</artifactId><version>{VERSION}</version><packaging>pom</packaging>
<dependencyManagement><dependencies><dependency>
<groupId>com.fasterxml.jackson.core</groupId><artifactId>jackson-databind</artifactId>
<version>{VERSION}</version></dependency></dependencies></dependencyManagement></project>""".encode()


class MavenThrottle(unittest.TestCase):
    def resolve(self, failures):
        attempts = []

        class Repository(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                if self.path == ARTIFACT_PATH:
                    attempts.append(self.path)
                    if len(attempts) <= failures:
                        self.send_response(429)
                        self.send_header("Retry-After", "1")
                        self.end_headers()
                        return
                    body = BOM
                elif self.path.startswith(ARTIFACT_PATH + "."):
                    algorithm = self.path.rsplit(".", 1)[1]
                    if algorithm not in ("sha1", "sha256", "sha512", "md5"):
                        self.send_error(404)
                        return
                    body = hashlib.new(algorithm, BOM).hexdigest().encode()
                else:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        host = os.environ.get("MAVEN_TEST_HOST", "127.0.0.1")
        bind = "127.0.0.1" if host == "127.0.0.1" else "0.0.0.0"
        with http.server.ThreadingHTTPServer((bind, 0), Repository) as server:
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                with tempfile.TemporaryDirectory(prefix="dre-maven-throttle-") as temp:
                    folder = Path(temp)
                    settings = folder / "settings.xml"
                    settings.write_text(f"""<settings><mirrors><mirror>
<id>fixture</id><mirrorOf>*</mirrorOf>
<url>http://{host}:{server.server_port}</url></mirror></mirrors></settings>""")
                    env = dict(os.environ, MAVEN_THROTTLE_DIR=temp)
                    result = subprocess.run(
                        [os.environ.get("MAVEN", "mvn"), "-B", "-V", "-C", "-s", str(settings),
                         f"-Dmaven.repo.local={folder / 'repository'}", "-f", str(POM), "validate"],
                        cwd=ROOT, env=env, text=True, stdout=subprocess.PIPE,
                        stderr=subprocess.STDOUT, timeout=30,
                    )
            finally:
                server.shutdown()
                thread.join()
        return result, len(attempts)

    def test_a_throttled_bom_recovers_within_the_retry_budget(self):
        result, attempts = self.resolve(4)
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(attempts, 5)

    def test_persistent_throttling_stops_after_five_retries(self):
        result, attempts = self.resolve(100)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(attempts, 6)


if __name__ == "__main__":
    unittest.main()
