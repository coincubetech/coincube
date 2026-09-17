"""Regression tests for the BTCB2 harness fetch scripts (launch-ga B4.1).

`fetch_electrs_blake2b.sh` must only ever hand the harness an electrs built
from the pinned commit's *tree*; it is driven in dry-run mode against a
throwaway git repository (no Cargo build, no network), covering the #386
review finding: with the cache checkout already at the pinned commit, an edit
to a tracked file survives `git checkout <commit>` and used to be compiled and
then trusted via the script's own marker.

`fetch_knots.sh` must re-verify a cached release with the current verifier on
every run and never reuse an extraction on the strength of a stored marker; it
is driven against a file:// release directory with a stand-in verifier that
records its invocations. The workflow's Knots cache key is asserted to be
bound to the verifier's inputs.
"""

import os
import stat
import subprocess
import textwrap

import pytest

SCRIPT = os.path.join(os.path.dirname(__file__), "tools", "fetch_electrs_blake2b.sh")


def _git(repo, *args):
    return subprocess.run(
        ["git", "-C", repo, *args], check=True, capture_output=True, text=True
    ).stdout.strip()


@pytest.fixture
def pinned_repo(tmp_path):
    """A throwaway upstream on branch `mempool` with one commit to pin."""
    repo = tmp_path / "upstream"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "mempool", str(repo)], check=True)
    _git(str(repo), "config", "user.email", "harness@example.invalid")
    _git(str(repo), "config", "user.name", "harness")
    (repo / "src" / "bin").mkdir(parents=True)
    (repo / "src" / "bin" / "electrs.rs").write_text("fn main() {}\n")
    (repo / "Cargo.toml").write_text('[package]\nname = "electrs"\nversion = "0.0.0"\n')
    _git(str(repo), "add", ".")
    _git(str(repo), "commit", "-q", "-m", "pinned")
    return str(repo), _git(str(repo), "rev-parse", "HEAD")


def run_fetch(cache_dir, repo, commit, dry_run=True):
    env = dict(
        os.environ,
        ELECTRS_BLAKE2B_TEST_REPO=repo,
        ELECTRS_BLAKE2B_TEST_COMMIT=commit,
        ELECTRS_BLAKE2B_TEST_BRANCH="mempool",
    )
    if dry_run:
        env["ELECTRS_BLAKE2B_DRY_RUN"] = "1"
    return subprocess.run(
        ["bash", SCRIPT, str(cache_dir)], env=env, capture_output=True, text=True
    )


def fake_binary(cache_dir, commit, version_line=None):
    """A stand-in `electrs` that answers `--version` like upstream's build does."""
    target = cache_dir / "target" / "release"
    target.mkdir(parents=True, exist_ok=True)
    binary = target / "electrs"
    version_line = version_line or f"mempool-electrs 0.0.0-dev-{commit[:7]}"
    binary.write_text(f"#!/bin/sh\necho '{version_line}'\n")
    binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
    return binary, version_line


def sha256_file(path):
    import hashlib

    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_marker(cache_dir, commit, binary, version_line):
    marker = cache_dir / "target" / f".built-{commit}-v2"
    marker.write_text(textwrap.dedent(f"""\
            commit={commit}
            sha256={sha256_file(binary)}
            version={version_line}
            """))
    return marker


def test_clean_checkout_proceeds_to_build(tmp_path, pinned_repo):
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    res = run_fetch(cache, repo, commit)
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(cache / "target" / "release" / "electrs")
    assert f"dry-run: would build {commit}" in res.stderr
    assert _git(str(cache / "src"), "rev-parse", "HEAD") == commit


def test_edited_tracked_file_at_pinned_head_is_refused_before_build(
    tmp_path, pinned_repo
):
    """The #386 review repro: HEAD pinned, src/bin/electrs.rs edited, script run."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0  # populate the cache
    edited = cache / "src" / "src" / "bin" / "electrs.rs"
    edited.write_text("fn main() { /* local edit */ }\n")
    assert _git(str(cache / "src"), "rev-parse", "HEAD") == commit

    res = run_fetch(cache, repo, commit, dry_run=False)
    assert res.returncode == 1
    assert "local changes" in res.stderr
    assert "src/bin/electrs.rs" in res.stderr
    assert res.stdout.strip() == "", "no binary path may be printed"
    assert not (cache / "target" / "release" / "electrs").exists()
    assert not (cache / "target").exists() or not list(
        (cache / "target").glob(".built-*")
    )
    # The developer's edit is preserved, not reset.
    assert "local edit" in edited.read_text()


def test_untracked_file_is_refused_like_an_edit(tmp_path, pinned_repo):
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0
    (cache / "src" / "src" / "bin" / "extra.rs").write_text("// dropped in\n")

    res = run_fetch(cache, repo, commit, dry_run=False)
    assert res.returncode == 1
    assert "src/bin/extra.rs" in res.stderr
    assert res.stdout.strip() == ""


def test_cached_binary_is_not_reused_when_source_is_modified(tmp_path, pinned_repo):
    """The marker is not self-certifying: a matching binary+marker is still
    refused when the checkout it claims to come from is no longer pristine."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0
    binary, version_line = fake_binary(cache, commit)
    write_marker(cache, commit, binary, version_line)

    # Clean tree: the recorded binary is reused.
    res = run_fetch(cache, repo, commit, dry_run=False)
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(binary)

    # Same binary and marker, but the source was edited: refused.
    (cache / "src" / "src" / "bin" / "electrs.rs").write_text("fn main() { 1 }\n")
    res = run_fetch(cache, repo, commit, dry_run=False)
    assert res.returncode == 1
    assert res.stdout.strip() == ""
    assert "local changes" in res.stderr


def test_marker_must_match_binary_digest_and_version(tmp_path, pinned_repo):
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0
    binary, version_line = fake_binary(cache, commit)
    marker = write_marker(cache, commit, binary, version_line)

    # A replaced binary (digest and reported version differ) is not reused.
    fake_binary(
        cache, commit, version_line=f"mempool-electrs 0.0.0-dev-{commit[:7]}(dirty)"
    )
    res = run_fetch(cache, repo, commit)
    assert res.returncode == 0 and "dry-run: would build" in res.stderr
    assert not binary.exists() and not marker.exists()

    # A marker of the pre-repair format is ignored (forces a checked rebuild).
    binary, version_line = fake_binary(cache, commit)
    (cache / "target" / f".built-{commit}").write_text(sha256_file(binary) + "\n")
    res = run_fetch(cache, repo, commit)
    assert res.returncode == 0 and "dry-run: would build" in res.stderr
    assert not binary.exists()


def test_wrong_commit_in_marker_is_not_reused(tmp_path, pinned_repo):
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0
    binary, version_line = fake_binary(cache, commit)
    marker = write_marker(cache, commit, binary, version_line)
    marker.write_text(marker.read_text().replace(commit, "0" * 40))
    res = run_fetch(cache, repo, commit)
    assert res.returncode == 0 and "dry-run: would build" in res.stderr
    assert not binary.exists()


# ── fetch_knots.sh ────────────────────────────────────────────────────────────

KNOTS_SCRIPT = os.path.join(os.path.dirname(__file__), "tools", "fetch_knots.sh")
WORKFLOW = os.path.join(
    os.path.dirname(__file__), "..", ".github", "workflows", "btcb2-regtest.yml"
)
FAKE_VERSION = "0.0.0.knots00000000"


def _host_suffix():
    import platform

    return {
        ("Darwin", "arm64"): "arm64-apple-darwin",
        ("Darwin", "x86_64"): "x86_64-apple-darwin",
        ("Linux", "x86_64"): "x86_64-linux-gnu",
        ("Linux", "aarch64"): "aarch64-linux-gnu",
    }[(platform.system(), platform.machine())]


@pytest.fixture
def fake_release(tmp_path):
    """A file:// release directory: archive with bin/bitcoind, manifest, signature."""
    release = tmp_path / "release"
    tree = tmp_path / "tree" / f"bitcoin-{FAKE_VERSION}" / "bin"
    tree.mkdir(parents=True)
    bitcoind = tree / "bitcoind"
    bitcoind.write_text(
        f"#!/bin/sh\necho 'Bitcoin Knots daemon version v{FAKE_VERSION}'\n"
    )
    bitcoind.chmod(bitcoind.stat().st_mode | stat.S_IXUSR)
    release.mkdir()
    archive = release / f"bitcoin-{FAKE_VERSION}-{_host_suffix()}.tar.gz"
    subprocess.run(
        [
            "tar",
            "-czf",
            str(archive),
            "-C",
            str(tmp_path / "tree"),
            f"bitcoin-{FAKE_VERSION}",
        ],
        check=True,
    )
    (release / "SHA256SUMS").write_text(f"{sha256_file(archive)}  {archive.name}\n")
    (release / "SHA256SUMS.asc").write_text(
        "-----BEGIN PGP SIGNATURE-----\nfake\n-----END PGP SIGNATURE-----\n"
    )
    return release, archive


@pytest.fixture
def fake_verifier(tmp_path):
    """Stand-in knots_verify: logs each invocation; exit code from a control file."""
    log = tmp_path / "verify.log"
    rc_file = tmp_path / "verify.rc"
    rc_file.write_text("0")
    script = tmp_path / "knots_verify"
    script.write_text(
        f'#!/bin/sh\necho "$1 $2 $3" >> "{log}"\nexit "$(cat "{rc_file}")"\n'
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR)
    return script, log, rc_file


def run_knots_fetch(cache_dir, release_dir, verifier):
    env = dict(
        os.environ,
        KNOTS_TEST_BASE_URL=f"file://{release_dir}",
        KNOTS_VERIFY_PATH=str(verifier),
    )
    return subprocess.run(
        ["bash", KNOTS_SCRIPT, FAKE_VERSION, str(cache_dir)],
        env=env,
        capture_output=True,
        text=True,
    )


def _invocations(log):
    return log.read_text().splitlines() if log.exists() else []


def test_knots_reuse_reverifies_with_the_current_verifier(
    tmp_path, fake_release, fake_verifier
):
    """No stored marker is trusted: the verifier runs on every invocation."""
    release, archive = fake_release
    verifier, log, _ = fake_verifier
    cache = tmp_path / "cache"
    first = run_knots_fetch(cache, release, verifier)
    assert first.returncode == 0, first.stderr
    bitcoind = cache / FAKE_VERSION / f"bitcoin-{FAKE_VERSION}" / "bin" / "bitcoind"
    assert first.stdout.strip() == str(bitcoind)
    assert len(_invocations(log)) == 1
    assert archive.name in _invocations(log)[0]
    assert not list((cache / FAKE_VERSION).glob(".verified*"))

    second = run_knots_fetch(cache, release, verifier)
    assert second.returncode == 0 and second.stdout.strip() == str(bitcoind)
    assert len(_invocations(log)) == 2, "reuse must re-run the verifier"
    assert not any(p.suffix == ".part" for p in (cache / FAKE_VERSION).iterdir())


def test_knots_tampered_extraction_is_replaced_from_the_archive(
    tmp_path, fake_release, fake_verifier
):
    release, _ = fake_release
    verifier, log, _ = fake_verifier
    cache = tmp_path / "cache"
    assert run_knots_fetch(cache, release, verifier).returncode == 0
    bitcoind = cache / FAKE_VERSION / f"bitcoin-{FAKE_VERSION}" / "bin" / "bitcoind"
    original = bitcoind.read_bytes()
    bitcoind.write_bytes(b"#!/bin/sh\necho substitute\n")

    res = run_knots_fetch(cache, release, verifier)
    assert res.returncode == 0 and res.stdout.strip() == str(bitcoind)
    assert bitcoind.read_bytes() == original


def test_knots_rejected_cache_is_refetched_then_fails_if_still_rejected(
    tmp_path, fake_release, fake_verifier
):
    """A verifier that now rejects the cached files (changed trust anchor, bad
    download) gets a fresh fetch; if that is rejected too, no path is printed."""
    release, _ = fake_release
    verifier, log, rc_file = fake_verifier
    cache = tmp_path / "cache"
    assert run_knots_fetch(cache, release, verifier).returncode == 0
    bitcoind = cache / FAKE_VERSION / f"bitcoin-{FAKE_VERSION}" / "bin" / "bitcoind"

    rc_file.write_text("1")
    res = run_knots_fetch(cache, release, verifier)
    assert res.returncode == 1
    assert res.stdout.strip() == "", "no bitcoind path may be printed"
    assert "no longer verify" in res.stderr
    # verifier ran on the cached set, then on the re-fetched set
    assert len(_invocations(log)) == 3
    assert not bitcoind.exists(), "a rejected release must not stay extracted"

    rc_file.write_text("0")
    res = run_knots_fetch(cache, release, verifier)
    assert res.returncode == 0 and res.stdout.strip() == str(bitcoind)


def test_knots_missing_verifier_is_fatal(tmp_path, fake_release):
    release, _ = fake_release
    env = dict(
        os.environ, KNOTS_TEST_BASE_URL=f"file://{release}", KNOTS_VERIFY_PATH=""
    )
    env["CARGO_TARGET_DIR"] = str(tmp_path / "nowhere")
    res = subprocess.run(
        ["bash", KNOTS_SCRIPT, FAKE_VERSION, str(tmp_path / "cache")],
        env=env,
        capture_output=True,
        text=True,
    )
    assert res.returncode == 2 and "knots_verify not built" in res.stderr
    assert res.stdout.strip() == ""


def test_workflow_knots_cache_key_is_bound_to_verifier_inputs():
    """The archives' cache key must change whenever the verifier's inputs do,
    exactly like the verifier's own cache key (parse-and-assert over the YAML)."""
    with open(WORKFLOW) as f:
        text = f.read()
    inputs = (
        "hashFiles('tests/tools/knots_verify/Cargo.lock', "
        "'tests/tools/knots_verify/src/main.rs', "
        "'coincube-gui/assets/knots_signing_key.asc')"
    )
    keys = [
        line.strip() for line in text.splitlines() if line.strip().startswith("key:")
    ]
    verifier_keys = [k for k in keys if "-knots-verify-" in k]
    knots_keys = [k for k in keys if "-knots-v" in k and "-knots-verify-" not in k]
    assert len(verifier_keys) == 1 and inputs in verifier_keys[0]
    assert len(knots_keys) == 1 and inputs in knots_keys[0]
    assert (
        "KNOTS_LEGACY_VERSION" in knots_keys[0]
        and "KNOTS_BLAKE2B_VERSION" in knots_keys[0]
    )
