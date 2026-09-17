"""Regression tests for tests/tools/fetch_electrs_blake2b.sh (launch-ga B4.1).

The script must only ever hand the BTCB2 harness an electrs built from the
pinned commit's *tree*. These tests drive it in dry-run mode against a
throwaway git repository (no Cargo build, no network), covering the review
finding on #386: with the cache checkout already at the pinned commit, an edit
to a tracked file survives `git checkout <commit>` and used to be compiled and
then trusted via the script's own marker.
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
