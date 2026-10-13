#!/usr/bin/env bash
# Exercise the documented policy against a real release, without extracting or executing it.
# Use the same downloaded bundle in negative checks: a missing API result isn't a policy test.
set -euo pipefail

repo=get-dre/dre
workflow="$repo/.github/workflows/release.yml"
tag=v0.4.1
archive=dre-0.4.1-linux-x86_64.tar.gz
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"

gh release download "$tag" --repo "$repo" --pattern "$archive"
gh attestation download "$archive" --repo "$repo"
bundles=(sha256*.jsonl)
[[ ${#bundles[@]} == 1 && -f ${bundles[0]} ]] || {
  echo 'Expected one downloaded attestation bundle' >&2
  exit 1
}
bundle=${bundles[0]}

# Check the documented online command, then the bundle used by all rejection tests.
gh attestation verify "$archive" --repo "$repo" --signer-workflow "$workflow"
gh attestation verify "$archive" --bundle "$bundle" \
  --repo "$repo" --signer-workflow "$workflow"
echo 'PASS: official release and downloaded bundle verify'

expect_rejection() {
  local label=$1
  shift
  local status=0
  gh attestation verify "$@" >rejection.log 2>&1 || status=$?
  if [[ $status != 1 ]]; then
    cat rejection.log >&2
    echo "FAIL: $label (expected verification failure, got exit $status)" >&2
    exit 1
  fi
  echo "PASS: $label rejected"
  cat rejection.log
}

# Use another repository under the same owner, so an owner-only policy cannot pass this test.
expect_rejection 'wrong repository' "$archive" --bundle "$bundle" \
  --repo get-dre/provenance-policy-negative-test --signer-workflow "$workflow"
expect_rejection 'wrong workflow' "$archive" --bundle "$bundle" \
  --repo "$repo" --signer-workflow "$repo/.github/workflows/security.yml"

cp "$archive" tampered.tar.gz
printf '\nchanged\n' >>tampered.tar.gz
expect_rejection 'tampered artifact' tampered.tar.gz --bundle "$bundle" \
  --repo "$repo" --signer-workflow "$workflow"
