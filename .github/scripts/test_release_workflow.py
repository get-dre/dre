#!/usr/bin/env python3
"""Guard release ordering and conditions that previously omitted core channels."""
from pathlib import Path
import re
import subprocess
import textwrap
import unittest

WORKFLOW = Path(__file__).parents[1] / "workflows/release.yml"
JOBS = dict(re.findall(r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)",
                       WORKFLOW.read_text(), re.M | re.S))


def field(job, name):
    value = re.search(rf"^    {name}: (.*(?:\n      [^\n]+)*)", JOBS[job], re.M)[1]
    return " ".join(value.removeprefix(">-").split())


def condition(job, *, published="success", managers="success", kind="core", dry=False,
              prerelease=False, build="success", attest="success", verified="success",
              event="push", tag="", skipped_ancestors=True):
    expression = field(job, "if")
    # GitHub otherwise applies implicit success() to the skipped Java ancestors too.
    if skipped_ancestors and "always()" not in expression:
        return False
    values = {
        "always()": "True", "needs.publish.result": repr(published),
        "needs.package-managers.result": repr(managers), "needs.plan.outputs.kind": repr(kind),
        "needs.plan.outputs.prerelease": repr(str(prerelease).lower()),
        "inputs.dry_run": repr(dry), "github.event_name": repr(event), "inputs.tag": repr(tag), "needs.build.result": repr(build),
        "needs.attest.result": repr(attest), "needs.crates-verify.result": repr(verified),
    }
    for key, value in values.items():
        expression = expression.replace(key, value)
    expression = re.sub(r"!(?!=)", " not ", expression.replace("&&", " and ").replace("||", " or "))
    return eval(expression, {"__builtins__": {}})


class ReleaseWorkflow(unittest.TestCase):
    def test_pdf_engine_uses_the_released_crate(self):
        root = WORKFLOW.parents[2]
        self.assertIn('fulgur = "0.41"', (root / 'plugins/document/Cargo.toml').read_text())
        self.assertIn('allow-git = []', (root / 'deny.toml').read_text())

    def test_release_source_is_gated_before_any_build_or_publish(self):
        self.assertIn('release_trust.py', JOBS['plan'])
        self.assertIn('GH_TOKEN:', JOBS['plan'])

    def test_registry_download_does_not_swallow_errors(self):
        self.assertNotIn('|| true', JOBS['registry'])
        self.assertIn('--download', JOBS['registry'])

    def test_jdbc_checksums_and_dependencies_are_gated_in_ci(self):
        ci = WORKFLOW.with_name('ci.yml').read_text()
        self.assertIn('mvn -B -C -f java/jdbc/pom.xml package', ci)
        self.assertIn('jdbc_dependencies.py check', ci)
        self.assertIn('mvn" -B -C -f "$JAVA_DIR/pom.xml" package', JOBS['build-jdbc'])

    def test_installer_curl_refuses_insecure_redirects(self):
        installer = WORKFLOW.parents[2] / 'install.sh'
        for line in installer.read_text().splitlines():
            if line.strip().startswith(('api() { curl', 'get_to() { curl')):
                self.assertIn("--proto '=https'", line)
                self.assertIn("--proto-redir '=https'", line)
                self.assertIn('--tlsv1.2', line)

    def test_cla_runs_only_first_party_code_from_the_default_branch(self):
        cla = WORKFLOW.with_name('cla.yml').read_text()
        self.assertNotIn('contributor-assistant/github-action', cla)
        self.assertNotIn('actions: write', cla)
        self.assertIn('ref: master', cla)
        self.assertIn('persist-credentials: false', cla)
        self.assertIn('python3 .github/scripts/cla.py', cla)

    def test_verification_runs_alongside_builds_without_publish_credentials(self):
        self.assertEqual(field("crates-verify", "needs"), field("build", "needs"))
        self.assertNotIn("id-token: write", JOBS["crates-verify"])
        self.assertNotIn("crates-io-auth-action", JOBS["crates-verify"])
        self.assertNotIn("env.DRY_RUN", JOBS["crates-verify"])

    def test_no_verify_upload_requires_verification_builds_and_provenance(self):
        needs = field("crates", "needs").strip("[]").split(", ")
        self.assertTrue({"plan", "build", "attest", "crates-verify"} <= set(needs))
        self.assertTrue(condition("crates"))
        for gate in ("build", "attest", "verified"):
            for status in ("failure", "cancelled", "skipped"):
                self.assertFalse(condition("crates", **{gate: status}))
        self.assertIn("if: env.DRY_RUN != 'true'", JOBS["crates"])

    def test_core_channels_survive_skipped_java_ancestors(self):
        for job in ("pypi", "package-managers", "site"):
            with self.subTest(job=job):
                self.assertTrue(condition(job))
                for status in ("failure", "cancelled", "skipped"):
                    self.assertFalse(condition(job, published=status))
                self.assertFalse(condition(job, kind="plugin"))
        self.assertTrue(condition("pypi", dry=True))
        self.assertFalse(condition("package-managers", dry=True))
        self.assertFalse(condition("site", dry=True))
        self.assertFalse(condition("site", prerelease=True))
        for status in ("failure", "cancelled", "skipped"):
            self.assertFalse(condition("site", managers=status))

    def test_site_only_recovery_never_publishes_wheels(self):
        args = dict(event="workflow_dispatch", tag="site", published="skipped", managers="skipped")
        self.assertTrue(condition("site", **args))
        self.assertFalse(condition("site", **args, dry=True))
        self.assertFalse(condition("site", **{**args, "event": "push"}))
        for tag in ("registry", "v0.11.1"):
            self.assertFalse(condition("site", **{**args, "tag": tag}))
        for tag in ("site", "registry"):
            self.assertFalse(condition("release-wheels", event="workflow_dispatch", tag=tag,
                                       skipped_ancestors=False))
        self.assertTrue(condition("release-wheels", event="workflow_dispatch", tag="v0.11.1",
                                  skipped_ancestors=False))

    def run_publish_script(self, job, existing="", fail=False):
        script = textwrap.dedent(re.search(
            r"^        run: \|\n((?:          [^\n]*\n|\n)*)", JOBS[job], re.M)[1])
        script = script.replace("${{ github.repository }}", "get-dre/dre")
        mock = r'''
        cargo() {
          if [[ "$1" == metadata ]]; then
            printf '%s\n' '{"packages":[{"name":"dre-protocol","version":"1.0.0"},{"name":"dre-core","version":"1.0.0"},{"name":"dre-cli","version":"1.0.0"}]}'
          else
            echo "cargo $*"
            [[ "$FAIL_VERIFY" != true ]]
          fi
        }
        curl() { [[ "$*" == *"/$EXISTING/"* && -n "$EXISTING" ]]; }
        '''
        return subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c",
             f"EXISTING={existing!r}; FAIL_VERIFY={str(fail).lower()}\n" + textwrap.dedent(mock) + script],
            capture_output=True, text=True)

    def test_verifies_unpublished_dependencies_together_and_propagates_failure(self):
        result = self.run_publish_script("crates-verify")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cargo publish --dry-run --locked -p dre-protocol -p dre-core -p dre-cli", result.stdout)
        self.assertNotEqual(self.run_publish_script("crates-verify", fail=True).returncode, 0)
        result = self.run_publish_script("crates-verify", existing="dre-protocol")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cargo publish --dry-run --locked -p dre-core -p dre-cli", result.stdout)

    def test_upload_does_not_rebuild_and_preserves_dependency_order(self):
        result = self.run_publish_script("crates")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            f"cargo publish -p {crate} --locked --no-verify"
            for crate in ("dre-protocol", "dre-core", "dre-cli")])
        result = self.run_publish_script("crates", existing="dre-protocol")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("cargo publish -p dre-protocol", result.stdout)
        self.assertNotEqual(self.run_publish_script("crates", fail=True).returncode, 0)


if __name__ == "__main__":
    unittest.main()
