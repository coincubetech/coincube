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

`Bitcoind(extra_args=...)` (introduced for the harness) must take a sequence
of complete argument strings and refuse a bare string instead of splitting it
into characters.

These tests need no node binaries and run wherever `pytest tests/` runs: the
six generic Functional Tests legs (the Knots leg runs `test_knots.py` only,
and the labelled BTCB2 workflow runs `test_btcb2_harness.py` only).
"""

import hashlib
import os
import pathlib
import stat
import subprocess
import textwrap

import pytest

from test_framework.bitcoind import Bitcoind

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


def run_fetch(cache_dir, repo, commit, dry_run=True, cwd=None, extra_env=None):
    """Drive the script against the throwaway repo. The test overrides are only
    honoured together with ELECTRS_BLAKE2B_DRY_RUN=1; refusal and reuse paths
    are reached before the dry-run point, so every scenario below runs with it."""
    env = dict(
        os.environ,
        ELECTRS_BLAKE2B_TEST_REPO=repo,
        ELECTRS_BLAKE2B_TEST_COMMIT=commit,
        ELECTRS_BLAKE2B_TEST_BRANCH="mempool",
    )
    env.pop("ELECTRS_BLAKE2B_DRY_RUN", None)  # parity with run_knots_fetch
    if dry_run:
        env["ELECTRS_BLAKE2B_DRY_RUN"] = "1"
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        ["bash", SCRIPT, str(cache_dir)],
        env=env,
        capture_output=True,
        text=True,
        cwd=cwd,
    )


def fake_binary(cache_dir, commit, version_line=None, target=None):
    """A stand-in `electrs` that answers `--version` like upstream's build does,
    under the default target or an explicit ELECTRS_BLAKE2B_TARGET_DIR."""
    target = (target or cache_dir / "target") / "release"
    target.mkdir(parents=True, exist_ok=True)
    binary = target / "electrs"
    version_line = version_line or f"mempool-electrs 0.0.0-dev-{commit[:7]}"
    binary.write_text(f"#!/bin/sh\necho '{version_line}'\n")
    binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
    return binary, version_line


def sha256_file(path):
    import hashlib

    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_marker(cache_dir, commit, binary, version_line, target=None):
    marker = (target or cache_dir / "target") / f".built-{commit}-v2"
    marker.write_text(textwrap.dedent(f"""\
            commit={commit}
            sha256={sha256_file(binary)}
            version={version_line}
            """))
    return marker


def snapshot(root):
    """Every entry under `root` with its kind and content digest (or link target),
    so a refused run can be shown to have created, removed or changed nothing."""
    out = {}
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        for name in dirnames + filenames:
            path = os.path.join(dirpath, name)
            rel = os.path.relpath(path, root)
            if os.path.islink(path):
                out[rel] = ("link", os.readlink(path))
            elif os.path.isdir(path):
                out[rel] = ("dir",)
            else:
                with open(path, "rb") as f:
                    out[rel] = ("file", hashlib.sha256(f.read()).hexdigest())
    return out


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

    res = run_fetch(cache, repo, commit)
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


def test_overrides_are_refused_outside_dry_run(tmp_path, pinned_repo):
    """An inherited ELECTRS_BLAKE2B_TEST_* variable must not redirect a real build."""
    repo, commit = pinned_repo
    res = run_fetch(tmp_path / "cache", repo, commit, dry_run=False)
    assert res.returncode == 2
    assert "only accepted with ELECTRS_BLAKE2B_DRY_RUN=1" in res.stderr
    assert res.stdout.strip() == ""
    assert not (tmp_path / "cache").exists(), "nothing may be cloned or built"


def test_ignored_file_in_checkout_is_refused(tmp_path, pinned_repo):
    """Ignored files are build inputs too (e.g. a stale .cargo/config.toml)."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0
    (cache / "src" / ".gitignore").write_text("*.log\n")
    _git(str(cache / "src"), "add", ".gitignore")
    # The clone carries no identity of its own (the fixture configured the
    # upstream repo only); CI runners have no global git identity either.
    _git(
        str(cache / "src"),
        "-c",
        "user.email=harness@example.invalid",
        "-c",
        "user.name=harness",
        "commit",
        "-q",
        "-m",
        "ignore logs",
    )
    ignored_commit = _git(str(cache / "src"), "rev-parse", "HEAD")
    (cache / "src" / "build.log").write_text("stale\n")
    assert "build.log" not in _git(str(cache / "src"), "status", "--porcelain")
    res = run_fetch(cache, repo, ignored_commit)
    assert res.returncode == 1
    assert "build.log" in res.stderr
    assert res.stdout.strip() == ""


def test_electrs_relative_cache_and_target_dirs_are_canonicalised(
    tmp_path, pinned_repo
):
    """A relative [cache-dir] or ELECTRS_BLAKE2B_TARGET_DIR used to be resolved
    under the checkout by the build subshell (`cd "$src"`), so the binary was
    written to <cache>/src/<cache>/target/… and never found at <cache>/target/….

    This is a *proxy* regression: dry-run returns before `cargo build`, so the
    failing build itself is not reachable here. It pins what the build depends
    on — every derived path is absolute and rooted where the caller meant."""
    repo, commit = pinned_repo
    res = run_fetch("rel-cache", repo, commit, cwd=str(tmp_path))
    assert res.returncode == 0, res.stderr
    printed = res.stdout.strip()
    assert os.path.isabs(printed)
    assert printed == str(tmp_path / "rel-cache" / "target" / "release" / "electrs")
    assert (tmp_path / "rel-cache" / "src" / ".git").is_dir()
    assert not (tmp_path / "rel-cache" / "src" / "rel-cache").exists()
    assert (
        f"would build {commit} from a clean checkout at {tmp_path / 'rel-cache' / 'src'}"
        in res.stderr
    )

    res = run_fetch(
        "rel-cache",
        repo,
        commit,
        cwd=str(tmp_path),
        extra_env={"ELECTRS_BLAKE2B_TARGET_DIR": "rel-target"},
    )
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(tmp_path / "rel-target" / "release" / "electrs")
    assert not (tmp_path / "rel-cache" / "src" / "rel-target").exists()


def _target_env(path):
    return {"ELECTRS_BLAKE2B_TARGET_DIR": str(path)}


def _assert_refused_inside_checkout(res):
    assert res.returncode == 2, res.stderr
    assert "ELECTRS_BLAKE2B_TARGET_DIR=" in res.stderr
    assert "inside the source checkout" in res.stderr
    assert "routes through the source checkout" not in res.stderr
    assert "unresolved symlink" not in res.stderr
    assert (
        "local changes" not in res.stderr
    ), "refused by the guard, not as dirty source"
    assert res.stdout.strip() == "", "no binary path may be printed"


def test_target_dir_equal_to_source_checkout_is_refused(tmp_path, pinned_repo):
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0
    before = snapshot(tmp_path)
    for spelling in (str(cache / "src"), str(cache / "src") + "/"):
        res = run_fetch(cache, repo, commit, extra_env=_target_env(spelling))
        _assert_refused_inside_checkout(res)
        assert snapshot(tmp_path) == before


def test_target_dir_under_source_checkout_is_refused_on_fresh_cache(
    tmp_path, pinned_repo
):
    """The reviewer's repro at 3e540317: on a fresh cache the script created the
    nested target first, `$src` was then non-empty and `git clone` died (128)."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    res = run_fetch(
        cache, repo, commit, extra_env=_target_env(cache / "src" / "target")
    )
    _assert_refused_inside_checkout(res)
    assert not cache.exists(), "nothing may be created or cloned"


def test_target_dir_under_source_checkout_is_refused_on_existing_checkout(
    tmp_path, pinned_repo
):
    """The reviewer's other repro: an empty nested target was accepted and, one
    build artefact later, refused as *dirty source* — the wrong diagnosis, and
    the restore hint would have told the developer to `git clean` their files."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0
    nested = cache / "src" / "target"

    # Not there yet: refused, not created; the checkout stays clean.
    before = snapshot(tmp_path)
    res = run_fetch(cache, repo, commit, extra_env=_target_env(nested))
    _assert_refused_inside_checkout(res)
    assert not nested.exists()
    assert snapshot(tmp_path) == before
    assert (
        _git(
            str(cache / "src"),
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored",
        )
        == ""
    )

    # The developer's files are already there: refused for the same reason and
    # left exactly where they are.
    (nested / "release").mkdir(parents=True)
    (nested / "release" / "electrs").write_text("developer's artefact\n")
    before = snapshot(tmp_path)
    for spelling in (
        str(nested),
        str(cache / "src" / "deeper" / "still"),
        "target",  # relative, resolved against a CWD inside the checkout
    ):
        cwd = str(cache / "src") if spelling == "target" else None
        res = run_fetch(cache, repo, commit, cwd=cwd, extra_env=_target_env(spelling))
        _assert_refused_inside_checkout(res)
        assert snapshot(tmp_path) == before


def test_target_dir_through_a_symlink_into_the_checkout_is_refused(
    tmp_path, pinned_repo
):
    """Aliases in both directions: a symlink *to* the checkout, and a checkout
    that is itself reached through a symlink (its physical tree lives elsewhere,
    so a textual prefix test against <cache>/src would miss it)."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0

    alias = tmp_path / "alias"
    alias.symlink_to(cache / "src", target_is_directory=True)
    before = snapshot(tmp_path)
    res = run_fetch(cache, repo, commit, extra_env=_target_env(alias / "target"))
    _assert_refused_inside_checkout(res)
    assert snapshot(tmp_path) == before

    physical = tmp_path / "physical-src"
    (cache / "src").rename(physical)
    (cache / "src").symlink_to(physical, target_is_directory=True)
    res = run_fetch(cache, repo, commit)  # still a valid checkout through the link
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(cache / "target" / "release" / "electrs")
    before = snapshot(tmp_path)
    res = run_fetch(cache, repo, commit, extra_env=_target_env(physical / "target"))
    _assert_refused_inside_checkout(res)
    assert snapshot(tmp_path) == before


def test_target_dir_via_dotdot_through_a_missing_component_is_refused(
    tmp_path, pinned_repo
):
    """The e99e86fa gate finding: a `..` that traverses a component which does not
    exist yet survived into the guard string verbatim, so `<x>/new/../cache/src/t`
    with `new` absent was not seen as `<x>/cache/src/t` — `mkdir -p` would have
    created `new`, the kernel would have resolved `..`, and Cargo would have
    written inside the checkout."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    spellings = (
        str(tmp_path / "new" / ".." / "cache" / "src" / "target"),
        str(tmp_path / "a" / "b" / ".." / ".." / "cache" / "src" / "deep" / "t"),
        os.path.join("a", "b", "..", "..", "cache", "src", "deep", "t"),  # relative
    )

    # Fresh cache: refused before anything is created or cloned.
    for spelling in spellings:
        res = run_fetch(
            cache, repo, commit, cwd=str(tmp_path), extra_env=_target_env(spelling)
        )
        _assert_refused_inside_checkout(res)
        assert not cache.exists(), spelling
        assert not (tmp_path / "new").exists() and not (tmp_path / "a").exists()

    # Existing checkout: refused by the guard, tree byte-identical.
    assert run_fetch(cache, repo, commit).returncode == 0
    before = snapshot(tmp_path)
    for spelling in spellings:
        res = run_fetch(
            cache, repo, commit, cwd=str(tmp_path), extra_env=_target_env(spelling)
        )
        _assert_refused_inside_checkout(res)
        assert snapshot(tmp_path) == before, spelling


def test_dotdot_after_an_existing_symlink_resolves_physically(tmp_path, pinned_repo):
    """Collapsing `..` lexically over the whole path would be wrong where the
    path exists: after a symlink, `..` is the physical parent. Both directions
    are pinned — a link out of the cache whose `..` lands in a valid external
    directory (accepted, and the printed path is the physical one Cargo will
    use), and a link into the checkout whose `..` lands back inside it
    (refused, although the lexical parent would be outside)."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0

    elsewhere = tmp_path / "elsewhere" / "realdir"
    elsewhere.mkdir(parents=True)
    (cache / "link").symlink_to(elsewhere, target_is_directory=True)
    before = snapshot(tmp_path)
    res = run_fetch(
        cache, repo, commit, extra_env=_target_env(cache / "link" / ".." / "t")
    )
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(
        tmp_path / "elsewhere" / "t" / "release" / "electrs"
    )
    assert snapshot(tmp_path) == before, "resolution creates nothing"

    into = tmp_path / "into"
    into.symlink_to(
        cache / "src" / "src", target_is_directory=True
    )  # inside the checkout
    before = snapshot(tmp_path)
    res = run_fetch(
        cache, repo, commit, extra_env=_target_env(into / ".." / "sub" / "t")
    )
    _assert_refused_inside_checkout(res)
    assert str(cache / "src" / "sub" / "t") in res.stderr
    assert snapshot(tmp_path) == before


def test_sibling_prefix_and_parent_targets_are_accepted(tmp_path, pinned_repo):
    """The guard is a path-component test, not a string-prefix test: `<cache>/src2`
    shares the prefix `<cache>/src` and must not be refused; `<cache>/../target`
    resolves through an existing directory to a valid external one."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0
    res = run_fetch(
        cache, repo, commit, extra_env=_target_env(cache / "src2" / "target")
    )
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(cache / "src2" / "target" / "release" / "electrs")
    assert not (cache / "src2").exists(), "a dry run creates no target"
    res = run_fetch(cache, repo, commit, extra_env=_target_env(cache / ".." / "target"))
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(tmp_path / "target" / "release" / "electrs")


def _assert_refused_through_checkout(res):
    """The 'routes through the checkout' refusal: the target would land outside
    today, but its path enters `<cache>/src` and climbs back out with `..`."""
    assert res.returncode == 2, res.stderr
    assert "ELECTRS_BLAKE2B_TARGET_DIR=" in res.stderr
    assert "routes through the source checkout" in res.stderr
    assert "inside the source checkout" not in res.stderr
    assert "unresolved symlink" not in res.stderr
    assert "local changes" not in res.stderr
    assert res.stdout.strip() == ""


def _assert_refused_unprovable(res):
    """The 'cannot prove it' refusal, as opposed to 'it is inside the checkout'."""
    assert res.returncode == 2, res.stderr
    assert "ELECTRS_BLAKE2B_TARGET_DIR=" in res.stderr
    assert "cannot be proved to stay outside the source checkout" in res.stderr
    assert "unresolved symlink component" in res.stderr
    assert "inside the source checkout" not in res.stderr
    assert "routes through the source checkout" not in res.stderr
    assert "local changes" not in res.stderr
    assert res.stdout.strip() == ""


def test_target_dir_through_an_unresolvable_symlink_is_refused(tmp_path, pinned_repo):
    """The 61de1982 gate finding: a symlink that cannot be followed at check time
    (dangling — including one that will come alive when the clone creates
    `<cache>/src` — or pointing at a file) was kept lexically, so the guard
    compared a string that stops meaning that once the link resolves. Refused as
    'cannot prove it', naming the component and its readlink target; a live link
    to a valid external directory is still accepted with the physical path."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"

    alias = tmp_path / "alias"
    alias.symlink_to(cache / "src")  # dangling: the checkout does not exist yet
    res = run_fetch(cache, repo, commit, extra_env=_target_env(alias / "target"))
    _assert_refused_unprovable(res)
    assert f"unresolved symlink component {alias} -> {cache / 'src'}" in res.stderr
    assert not cache.exists(), "nothing may be created or cloned"

    harmless = tmp_path / "harmless"
    harmless.symlink_to(tmp_path / "nowhere")
    res = run_fetch(cache, repo, commit, extra_env=_target_env(harmless / "target"))
    _assert_refused_unprovable(res)
    assert (
        f"unresolved symlink component {harmless} -> {tmp_path / 'nowhere'}"
        in res.stderr
    )
    assert not cache.exists()

    afile = tmp_path / "afile"
    afile.write_text("")
    filelink = tmp_path / "filelink"
    filelink.symlink_to(afile)
    res = run_fetch(cache, repo, commit, extra_env=_target_env(filelink / "target"))
    _assert_refused_unprovable(res)
    assert f"unresolved symlink component {filelink} -> {afile}" in res.stderr
    assert not cache.exists()

    realdir = tmp_path / "elsewhere" / "realdir"
    realdir.mkdir(parents=True)
    live = tmp_path / "live"
    live.symlink_to(realdir, target_is_directory=True)
    res = run_fetch(cache, repo, commit, extra_env=_target_env(live / "t"))
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(realdir / "t" / "release" / "electrs")
    assert (cache / "src" / ".git").is_dir()


def test_target_dir_routed_through_the_checkout_is_refused_even_if_it_climbs_out(
    tmp_path, pinned_repo
):
    """Below `<cache>/src` the clone, not `mkdir -p`, creates the missing
    components — as whatever the pinned commit tracks, including a symlink. So
    a spelling that enters the checkout and climbs back out with `..` resolves
    to one place at check time and to another once the link exists:
    `<cache>/src/foo/../../../elsewhere` with a tracked `foo -> bin/x/y` is
    `<x>/elsewhere` before the clone and `<cache>/src/elsewhere` after it. The
    check therefore runs at every step of the resolution: reaching the
    checkout at any point refuses, on a fresh cache and on an existing one."""
    repo, commit = pinned_repo
    (pathlib.Path(repo) / "bin" / "x" / "y").mkdir(parents=True)
    (pathlib.Path(repo) / "bin" / "x" / "y" / "e.rs").write_text(
        ""
    )  # git tracks files, not dirs
    (pathlib.Path(repo) / "foo").symlink_to(os.path.join("bin", "x", "y"))
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "tracked symlink")
    commit = _git(repo, "rev-parse", "HEAD")
    cache = tmp_path / "cache"
    through = cache / "src" / "foo" / ".." / ".." / ".." / "elsewhere"

    # Fresh cache: the clone would create `foo` as a symlink and move the target.
    res = run_fetch(cache, repo, commit, extra_env=_target_env(through))
    _assert_refused_through_checkout(res)
    assert (
        f"(reaches {cache / 'src'}, then climbs back out to {tmp_path / 'elsewhere'})"
        in res.stderr
    )
    assert not cache.exists(), "nothing may be created or cloned"
    assert not (tmp_path / "elsewhere").exists()

    # Existing checkout: `foo` is live now, so the same spelling resolves
    # physically to where the clone really put it — inside the checkout — and
    # is refused as such. Byte-identical either way.
    assert run_fetch(cache, repo, commit).returncode == 0
    assert (cache / "src" / "foo").is_symlink() and (cache / "src" / "foo").is_dir()
    before = snapshot(tmp_path)
    res = run_fetch(cache, repo, commit, extra_env=_target_env(through))
    _assert_refused_inside_checkout(res)
    assert f"resolves to {cache / 'src' / 'elsewhere'}, inside" in res.stderr
    assert snapshot(tmp_path) == before
    # ...while spellings that never enter the checkout are unaffected.
    for spelling, printed in (
        (cache / "src2" / "target", cache / "src2" / "target"),
        (cache / "nope" / ".." / "target", cache / "target"),
        (cache / ".." / "target", tmp_path / "target"),
    ):
        res = run_fetch(cache, repo, commit, extra_env=_target_env(spelling))
        assert res.returncode == 0, res.stderr
        assert res.stdout.strip() == str(printed / "release" / "electrs")
    assert snapshot(tmp_path) == before


def test_target_dir_climbing_out_of_the_checkout_is_refused_as_routed_through(
    tmp_path, pinned_repo
):
    """Behaviour change from 61de1982, deliberate: `<cache>/src/../target` was
    accepted there (it lands at `<cache>/target` today) and is now refused with
    the routes-through message, because the class rule is that a target may not
    pass through the checkout at all — not that it may not end up inside it."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    spelling = cache / "src" / ".." / "target"

    res = run_fetch(cache, repo, commit, extra_env=_target_env(spelling))
    _assert_refused_through_checkout(res)
    assert not cache.exists()

    assert run_fetch(cache, repo, commit).returncode == 0
    before = snapshot(tmp_path)
    res = run_fetch(cache, repo, commit, extra_env=_target_env(spelling))
    _assert_refused_through_checkout(res)
    assert (
        f"(reaches {cache / 'src'}, then climbs back out to {cache / 'target'})"
        in res.stderr
    )
    assert snapshot(tmp_path) == before
    # The equivalent spelling that never enters the checkout is still accepted.
    res = run_fetch(cache, repo, commit, extra_env=_target_env(cache / "target"))
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(cache / "target" / "release" / "electrs")


def test_external_target_dir_is_accepted_and_reused(tmp_path, pinned_repo):
    """A valid ELECTRS_BLAKE2B_TARGET_DIR outside the checkout still works end to
    end: the binary path is derived under it, a checked build recorded there is
    reused from there, and a later refused run leaves it byte-identical."""
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    external = tmp_path / "external-target"  # deliberately not created here
    res = run_fetch(cache, repo, commit, extra_env=_target_env(external))
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(external / "release" / "electrs")
    assert (cache / "src" / ".git").is_dir()
    assert not (cache / "src" / "external-target").exists()

    binary, version_line = fake_binary(cache, commit, target=external)
    write_marker(cache, commit, binary, version_line, target=external)
    res = run_fetch(cache, repo, commit, extra_env=_target_env(external))
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(binary)
    assert "dry-run" not in res.stderr, "reused, not rebuilt"

    before = snapshot(tmp_path)
    res = run_fetch(
        cache, repo, commit, extra_env=_target_env(cache / "src" / "target")
    )
    _assert_refused_inside_checkout(res)
    assert snapshot(tmp_path) == before


def test_untracked_file_is_refused_like_an_edit(tmp_path, pinned_repo):
    repo, commit = pinned_repo
    cache = tmp_path / "cache"
    assert run_fetch(cache, repo, commit).returncode == 0
    (cache / "src" / "src" / "bin" / "extra.rs").write_text("// dropped in\n")

    res = run_fetch(cache, repo, commit)
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
    res = run_fetch(cache, repo, commit)
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == str(binary)

    # Same binary and marker, but the source was edited: refused.
    (cache / "src" / "src" / "bin" / "electrs.rs").write_text("fn main() { 1 }\n")
    res = run_fetch(cache, repo, commit)
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


def run_knots_fetch(cache_dir, release_dir, verifier, test_mode=True, cwd=None):
    """Drive the script against the file:// release with the stand-in verifier.
    Both overrides are honoured only with KNOTS_TEST_MODE=1."""
    env = dict(
        os.environ,
        KNOTS_TEST_BASE_URL=f"file://{release_dir}",
        KNOTS_TEST_VERIFY_PATH=str(verifier),
    )
    env.pop("KNOTS_TEST_MODE", None)
    if test_mode:
        env["KNOTS_TEST_MODE"] = "1"
    return subprocess.run(
        ["bash", KNOTS_SCRIPT, FAKE_VERSION, str(cache_dir)],
        env=env,
        capture_output=True,
        text=True,
        cwd=cwd,
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
    # The verifier that vouched is always named on stderr (the resolution is
    # environment-dependent in real runs, via CARGO_TARGET_DIR).
    assert f"verifier: {verifier}" in first.stderr

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


def test_knots_overrides_are_refused_outside_test_mode(
    tmp_path, fake_release, fake_verifier
):
    """An inherited KNOTS_TEST_* variable must neither redirect the download nor
    replace the verifier: the script exits before fetching or creating anything."""
    release, _ = fake_release
    verifier, log, _ = fake_verifier
    cache = tmp_path / "cache"
    res = run_knots_fetch(cache, release, verifier, test_mode=False)
    assert res.returncode == 2
    assert "only accepted with KNOTS_TEST_MODE=1" in res.stderr
    assert res.stdout.strip() == ""
    assert not cache.exists()
    assert _invocations(log) == []


def test_knots_test_mode_never_falls_back_to_a_real_verifier(tmp_path, fake_release):
    """Hermetic by construction: in test mode the stand-in verifier is mandatory,
    so a repo-local knots_verify build can never be picked up by a test."""
    release, _ = fake_release
    cache = tmp_path / "cache"
    res = run_knots_fetch(cache, release, tmp_path / "no-such-verifier")
    assert res.returncode == 2
    assert "not an executable" in res.stderr
    assert res.stdout.strip() == ""
    assert not (cache / FAKE_VERSION).exists(), "nothing may be downloaded"

    env = dict(os.environ, KNOTS_TEST_MODE="1", KNOTS_TEST_BASE_URL=f"file://{release}")
    env.pop("KNOTS_TEST_VERIFY_PATH", None)
    res = subprocess.run(
        ["bash", KNOTS_SCRIPT, FAKE_VERSION, str(cache)],
        env=env,
        capture_output=True,
        text=True,
    )
    assert res.returncode == 2 and "requires both" in res.stderr


def test_knots_relative_cache_dir_is_canonicalised(
    tmp_path, fake_release, fake_verifier
):
    """The documented [cache-dir] argument may be relative; the script cd's into
    it, so it must be resolved first and the printed path must work from anywhere."""
    release, _ = fake_release
    verifier, _, _ = fake_verifier
    res = run_knots_fetch("rel-cache", release, verifier, cwd=str(tmp_path))
    assert res.returncode == 0, res.stderr
    printed = res.stdout.strip()
    assert os.path.isabs(printed)
    assert printed == str(
        tmp_path
        / "rel-cache"
        / FAKE_VERSION
        / f"bitcoin-{FAKE_VERSION}"
        / "bin"
        / "bitcoind"
    )
    assert os.access(printed, os.X_OK)
    # Reuse from another cwd resolves the same extraction rather than nesting one.
    res = run_knots_fetch(str(tmp_path / "rel-cache"), release, verifier, cwd="/")
    assert res.returncode == 0 and res.stdout.strip() == printed
    assert not (tmp_path / "rel-cache" / FAKE_VERSION / "rel-cache").exists()


def test_knots_relative_cargo_target_dir_reaches_the_verifier_after_the_cd(
    tmp_path, fake_release, fake_verifier
):
    """Outside test mode the verifier is located through CARGO_TARGET_DIR. The
    candidate is tested against the caller's cwd but invoked after `cd "$dest"`,
    so a relative CARGO_TARGET_DIR used to select a verifier and then fail to
    find it (exit 127). Hermetic: the release files are pre-seeded in the cache
    so nothing is downloaded, no KNOTS_TEST_* variable is set, and any
    unexpected download is sent to a closed local proxy port."""
    import shutil

    release, archive = fake_release
    verifier, log, _ = fake_verifier
    target = tmp_path / "target" / "release"
    target.mkdir(parents=True)
    shutil.copy2(verifier, target / "knots_verify")
    cache = tmp_path / "cache"
    dest = cache / FAKE_VERSION
    dest.mkdir(parents=True)
    for name in (archive.name, "SHA256SUMS", "SHA256SUMS.asc"):
        shutil.copy2(release / name, dest / name)

    env = dict(os.environ, CARGO_TARGET_DIR="target")
    for var in ("KNOTS_TEST_MODE", "KNOTS_TEST_BASE_URL", "KNOTS_TEST_VERIFY_PATH"):
        env.pop(var, None)
    env.update(
        https_proxy="http://127.0.0.1:1",
        HTTPS_PROXY="http://127.0.0.1:1",
        http_proxy="http://127.0.0.1:1",
        no_proxy="",
    )
    res = subprocess.run(
        ["bash", KNOTS_SCRIPT, FAKE_VERSION, str(cache)],
        env=env,
        capture_output=True,
        text=True,
        cwd=str(tmp_path),
    )
    assert res.returncode == 0, res.stderr
    assert f"verifier: {target / 'knots_verify'}" in res.stderr
    assert _invocations(log) == [f"{archive.name} SHA256SUMS SHA256SUMS.asc"]
    assert res.stdout.strip() == str(
        dest / f"bitcoin-{FAKE_VERSION}" / "bin" / "bitcoind"
    )


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


# ── Bitcoind(extra_args=...) ─────────────────────────────────────────────────


def test_bitcoind_extra_args_are_complete_argument_strings(tmp_path):
    """A bare string used to be iterated into one argument per character."""
    node_dir = tmp_path / "node"
    node_dir.mkdir()
    with pytest.raises(TypeError, match="sequence of complete argument strings"):
        Bitcoind(str(node_dir), extra_args="-testactivationheight=blake2b@110")
    assert list(node_dir.iterdir()) == [], "refused before any side effect"

    schedule = ["-testactivationheight=blake2b@110", "-rdtsexpiry=4102444800"]
    node = Bitcoind(str(node_dir), extra_args=schedule)
    assert node.cmd_line[0] == node.bitcoind_path
    assert node.cmd_line[-2:] == schedule

    plain = Bitcoind(str(node_dir), extra_args=None)
    assert plain.cmd_line == node.cmd_line[:-2]
    assert plain.cmd_line[-1] == "-debugexclude=tor"
