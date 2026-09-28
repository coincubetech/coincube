# Contributing to Coincube

Anyone is welcome to contribute to Coincube regardless of any arbitrary criterion. Contribution are
only judged based on their technical relevance and quality.

Note that the development of Bitcoin software requires a high level of rigor, so it could take some
time (and backs and forths) to polish a contribution before it's ready for merge.


## Communication

Most of the communication is done on GitHub.

If you plan to contribute a non-trivial change, consider discussing it in the IRC channel or in a
Github issue before going forward with the implementation.


## Looking for contributions

If you are looking for first time contributions, you can `git grep` for `FIXME`s and `TODO`s
as well as checking out the [good first issues](https://github.com/coincubetech/coincube/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
on the issue tracker.


# Workflow

The codebase is maintained using the "contributor workflow" where everyone
without exception contributes patch proposals using "pull requests" (PRs). This
facilitates social contribution, easy testing and peer review.

In general, [commits should be atomic](https://en.wikipedia.org/wiki/Atomic_commit#Atomic_commit_convention)
and diffs should be easy to read. For this reason, do not mix any formatting
fixes or code moves with actual code changes.
Make sure each individual commit is hygienic: that it builds successfully
on its own without warnings, errors, regressions, or test failures.

Commit messages should be verbose by default consisting of a short subject line,
a blank line and detailed explanatory text as separate paragraph(s), unless the
title alone is self-explanatory. Commit messages should be helpful to people
reading your code in the future, so explain the reasoning for your decisions. For
more guidelines about writing commit messages, see this [blog post](https://cbea.ms/git-commit/).

If your pull request contains fixup commits (commits that change the same line of code repeatedly) or too fine-grained
commits, you may be asked to [squash](https://git-scm.com/docs/git-rebase#_interactive_mode) your commits
before it will be merged.

Patchsets should always be focused. For example, a pull request could add a
feature, fix a bug, or refactor code; but not a mixture. Please also avoid super
pull requests which attempt to do too much, are overly large, or overly complex
as this makes review difficult. Instead, prefer opening different focused pull requests.

Anyone may participate in peer review which is expressed by comments in the pull
request. Typically reviewers will review the code for obvious errors, as well as
test out the patch set and opine on the technical merits of the patch. PR should
be reviewed first on the conceptual level before focusing on code style or grammar
fixes.

Any new contributed feature must come with tests. Preferably both an integration/functional tests
demonstrating its usage in a blackbox manner (for instance calling an RPC command under different
conditions), as well as unit tests exercising specific parts of the logic (for instance a database
query).


# Code

## Rust version

The Rust toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml), currently **1.97.1**.
`rustup` selects it automatically when you build anywhere in this repository, and CI lints and tests
with that same version. Treat that file as the source of truth rather than the number quoted here.

There is no separately supported older Rust version: the crates do not declare a `rust-version` in
their manifests and CI does not exercise one, so building with an older toolchain is untested.

When bumping the pin, note that `rust-toolchain.toml` is not the only place the version appears: CI
jobs that install a toolchain explicitly carry their own copy, and those copies do not follow the
file. Find every one of them before you start:

```
git grep -n 'toolchain: [0-9]' .github/workflows/
```

Update them together with `rust-toolchain.toml`, then run `cargo fmt -- --check` and
`cargo clippy --all-targets -- -D warnings`. A new stable routinely adds default lints, and clippy is
a hard CI gate, so expect to fix some.

`.github/workflows/coverage.yml` is deliberately excluded from that sweep: it pins its own nightly
for `cargo-llvm-cov` and is independent of the stable pin.

## Style

To avoid endless bikeshedding, just use [`rustfmt`](https://github.com/rust-lang/rustfmt).

[Clippy](https://github.com/rust-lang/rust-clippy) is also often your friend.

### Large type lint policy

GUI and daemon builds enable `large_enum_variant` and `result_large_err`.
The workspace `clippy.toml` permits a 4096-byte enum variant size difference
and errors below 256 bytes; CI's `-D warnings` enforces both. Prefer boxing a
bulky payload over raising these thresholds or adding an unexplained allow.
The Tab State layout test also caps its total size at 8 KiB, since relative
variant-size linting cannot catch all variants growing together.

The initial #482 measurement on macOS ARM64 with Rust 1.94 found 35 unique
enum warnings (34 GUI source sites and one generated protobuf site), plus
31 result warnings (13 GUI and 18 daemon). Existing result payloads were
176–192 bytes. The measured thresholds let all existing types pass, including
six previously suppressed enum sites, while restoring inline `App.panels`
made Clippy reject a 33,312-byte Tab State. Sizes and warning counts can vary
by toolchain and target; the pinned CI build remains the merge gate.
