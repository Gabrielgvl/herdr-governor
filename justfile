# justfile — `just ci` is the single entrypoint (docs/guardrails.md).
# Every recipe fails closed: a missing tool, config, or script is a broken
# scaffold, not a skipped check. No `|| true`, no `-` prefixes on required steps.

set shell := ["bash", "-euo", "pipefail", "-c"]

# list recipes
default:
    @just --list

# everything CI runs — the definition of done for agents
ci: fmt lint test deny hygiene wf guard guard-selftest doc purity schema-check test-inventory mutants-diff
    @echo "ci: all green"

fmt:
    cargo fmt --check

lint:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

test:
    cargo nextest run --locked --status-level fail --final-status-level fail

deny:
    cargo deny check

hygiene:
    typos
    tombi format --check .
    tombi lint .
    cargo sort --check --workspace
    cargo shear
    shellcheck -S warning scripts/*.sh .githooks/*
    shfmt -d -i 2 -ci scripts/ .githooks/
    find . \( -path ./target -o -path ./.git -o -name 'mutants.out*' \) -prune -o -type f -name '*.rs' -print0 | xargs -0 wc -l | awk '$1 > 500 && $2 != "total" { print; bad = 1 } END { exit bad }'

wf:
    actionlint .github/workflows/*.yml
    zizmor --offline .github/workflows/

guard:
    scripts/check-lint-integrity.sh
    scripts/check-protected-diff.sh

# the gates' own adversarial suite — without it a gate regression ships green
guard-selftest:
    python3 scripts/test_guardrails.py

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked

# governor-core stays pure: allowed dep set + no I/O/unsafe in core sources
purity:
    scripts/check-core-purity.sh

# herdr api schema fixture: exists, parses, pinned protocol revision, and
# byte-matches its .sha256 sidecar — a missing fixture or pin is a FAIL,
# so `just ci` reports the same verdict locally and in CI
schema-check:
    scripts/check-herdr-schema.sh

# live fixture drift against `herdr api schema --json` — kept out of ci;
# FAILs when herdr is not on PATH
schema-live:
    scripts/check-herdr-schema.sh --live

# Phase 2 contract suite — offline executable checks pinning each confirmed
# evidence behavior to its committed fixture (tests/fixtures/contract/);
# fails closed on missing or malformed fixtures, contacts no live system.
# Kept out of ci while the evidence set is still accumulating (Phase 2).
contract:
    python3 -B tests/contract/contract_tests.py

# BASE-dependent gates: BASE defaults to `git merge-base HEAD origin/main`
# and both recipes FAIL when it cannot be resolved — a missing base means the
# check cannot honestly run, so it is never skipped. Before the first push
# (no origin yet) bootstrap with an explicit base: `just test-inventory <sha>`.
# A BASE commit without Cargo.toml (a pre-workspace seed) yields an empty
# BASE inventory and the ratchet passes — no base test can be missing; any
# other `cargo nextest list` failure at BASE or HEAD still FAILs.

# test-inventory ratchet: every test in BASE's `cargo nextest list` must still
# be present in HEAD's — catches deleted, renamed, unwired, and filtered tests
test-inventory base="":
    #!/usr/bin/env bash
    set -euo pipefail
    base="{{base}}"
    if [ -z "$base" ]; then
        if ! base="$(git merge-base HEAD origin/main 2>/dev/null)" || [ -z "$base" ]; then
            echo "test-inventory: no merge-base with origin/main and no BASE argument" >&2
            echo "test-inventory: bootstrap with 'just test-inventory <sha>'" >&2
            exit 1
        fi
    fi
    echo "test-inventory: BASE=$base"
    tmp="$(mktemp -d)"
    trap 'git worktree remove --force "$tmp/base" 2>/dev/null; rm -rf "$tmp"; git worktree prune' EXIT
    git worktree add --detach "$tmp/base" "$base"
    inv_args=()
    if [ -f "$tmp/base/Cargo.toml" ]; then
        (cd "$tmp/base" && cargo nextest list --locked --message-format json >"$tmp/base.jsonl")
    else
        echo "test-inventory: BASE has no Cargo.toml — treating BASE inventory as empty"
        printf '{"rust-suites":{}}\n' >"$tmp/base.jsonl"
        inv_args=(--allow-empty-base)
    fi
    cargo nextest list --locked --message-format json >"$tmp/head.jsonl"
    scripts/check-test-inventory.sh "${inv_args[@]}" "$tmp/base.jsonl" "$tmp/head.jsonl"

# mutation gate on diffs: core-src diffs run in-diff mutants (a zero-entry
# --list reports and passes), test-only diffs trigger a full governor-core run
mutants-diff base="":
    #!/usr/bin/env bash
    set -euo pipefail
    base="{{base}}"
    if [ -z "$base" ]; then
        if ! base="$(git merge-base HEAD origin/main 2>/dev/null)" || [ -z "$base" ]; then
            echo "mutants-diff: no merge-base with origin/main and no BASE argument" >&2
            echo "mutants-diff: bootstrap with 'just mutants-diff <sha>'" >&2
            exit 1
        fi
    fi
    echo "mutants-diff: BASE=$base"
    scripts/mutants-diff.sh "$base"

# informational only — no floors until real code lands and floors can be honest
cov:
    cargo llvm-cov nextest --locked --summary-only

# install the pinned tool manifest
tools:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo install --locked cargo-deny@0.20.2 cargo-nextest@0.9.146 cargo-sort@2.1.4 typos-cli@1.50.3 cargo-shear@1.14.0 cargo-llvm-cov@0.9.1 cargo-mutants@27.1.0 zizmor@1.30.1 ripgrep@14.1.1
    # tombi==1.5.5 — pipx is the plan path; `uv tool install` is the PEP
    # 668-safe fallback when pipx is absent (`pip --user` is blocked there)
    if ! tombi --version 2>/dev/null | grep -q 'tombi 1\.5\.5'; then
        if command -v pipx >/dev/null 2>&1; then
            pipx install --force tombi==1.5.5
        else
            uv tool install --force tombi==1.5.5
        fi
    fi
    tombi --version | grep -q 'tombi 1\.5\.5'
    # shellcheck 0.11.0 — apt first; when the pin still isn't met, install the
    # pinned GitHub release binary into ~/.local/bin, sha256-verified against
    # the digests published on the v0.11.0 release
    if ! shellcheck --version 2>/dev/null | grep -q 'version: 0\.11\.0'; then
        if ! sudo apt-get install -y shellcheck; then
            echo "tools: apt shellcheck unavailable; falling back to pinned release binary" >&2
        fi
    fi
    if ! shellcheck --version 2>/dev/null | grep -q 'version: 0\.11\.0'; then
        case "$(uname -s)-$(uname -m)" in
            Linux-x86_64)   sc_platform=linux.x86_64;   sc_sha=8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198 ;;
            Linux-aarch64)  sc_platform=linux.aarch64;  sc_sha=12b331c1d2db6b9eb13cfca64306b1b157a86eb69db83023e261eaa7e7c14588 ;;
            Darwin-arm64)   sc_platform=darwin.aarch64; sc_sha=56affdd8de5527894dca6dc3d7e0a99a873b0f004d7aabc30ae407d3f48b0a79 ;;
            Darwin-x86_64)  sc_platform=darwin.x86_64;  sc_sha=3c89db4edcab7cf1c27bff178882e0f6f27f7afdf54e859fa041fca10febe4c6 ;;
            *) echo "tools: no pinned shellcheck 0.11.0 binary for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
        esac
        sc_tmp="$(mktemp -d)"
        trap 'rm -rf "$sc_tmp"' EXIT
        curl -fsSL -o "$sc_tmp/shellcheck.tar.xz" \
            "https://github.com/koalaman/shellcheck/releases/download/v0.11.0/shellcheck-v0.11.0.${sc_platform}.tar.xz"
        (cd "$sc_tmp" && echo "$sc_sha  shellcheck.tar.xz" | {
            command -v sha256sum >/dev/null 2>&1 && sha256sum -c - || shasum -a 256 -c -
        })
        tar -xJf "$sc_tmp/shellcheck.tar.xz" -C "$sc_tmp"
        mkdir -p "$HOME/.local/bin"
        install -m755 "$sc_tmp/shellcheck-v0.11.0/shellcheck" "$HOME/.local/bin/shellcheck"
        rm -rf "$sc_tmp"
        trap - EXIT
    fi
    shellcheck --version | grep -q 'version: 0\.11\.0'
    # shfmt 3.14.1 — go-installed binaries print `v3.14.1`, apt's print
    # `3.x.y`; the check accepts the optional v prefix. The go fallback pins
    # exactly and runs whenever the version check fails (apt usually lags)
    if ! shfmt --version 2>/dev/null | grep -Eqx 'v?3\.14\.1'; then
        if ! sudo apt-get install -y shfmt; then
            echo "tools: apt shfmt unavailable; falling back to pinned go install" >&2
        fi
        shfmt --version 2>/dev/null | grep -Eqx 'v?3\.14\.1' ||
            GOBIN="$HOME/.local/bin" go install mvdan.cc/sh/v3/cmd/shfmt@v3.14.1
    fi
    shfmt --version | grep -Eqx 'v?3\.14\.1'
    # actionlint v1.7.12 (already on this host; go-install fallback)
    command -v actionlint >/dev/null 2>&1 ||
        GOBIN="$HOME/.local/bin" go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12

# install repo git hooks once per clone
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/*

# nightly budget: informational; legs self-activate as their targets land
nightly: mutants fuzz miri
    @echo "nightly: done (informational)"

# mutation testing over governor-core — the pure crate holds all the decision
# logic worth mutating; herdr-governor is I/O glue (spec §9)
mutants:
    cargo mutants -p governor-core --timeout 300 -vV

# Detection rule shared with the nightly job (which calls this recipe):
# the recipe runs `cargo test --locked fuzz` — a test-name substring filter —
# so a detectable target is exactly what that command can run: a `fn *fuzz*`
# or `mod *fuzz*` under a member's src//tests/ (embedded bolero/property
# #[test] targets) or a bolero dep expected to produce them. The cargo-fuzz
# layout is not runnable this way — it fails loudly instead of silently passing.
# Runs the embedded fuzz targets under the 10-min budget
fuzz:
    #!/usr/bin/env bash
    set -euo pipefail
    shopt -s nullglob
    cf_targets=(fuzz/fuzz_targets/*.rs */fuzz/fuzz_targets/*.rs)
    if [ "${#cf_targets[@]}" -gt 0 ]; then
        echo "fuzz: */fuzz_targets/*.rs needs cargo-fuzz, which is not in the pinned manifest" >&2
        echo "fuzz: add cargo-fuzz to 'just tools' or port the targets to embedded #[test] fns named *fuzz*" >&2
        exit 1
    fi
    detected=0
    # members are top-level dirs (pinned layout); `grep -r` on a missing dir
    # still exits 0 on a match elsewhere, so a not-yet-created member tests/
    # can't false-skip embedded targets in src/
    if grep -rEq '(fn|mod)[[:space:]]+[A-Za-z0-9_]*fuzz[A-Za-z0-9_]*' governor-core herdr-governor 2>/dev/null; then
        detected=1
    elif grep -rq bolero Cargo.toml governor-core herdr-governor 2>/dev/null; then
        detected=1
    fi
    if [ "$detected" -eq 0 ]; then
        echo "fuzz: no fuzz targets exist yet; skipping"
        exit 0
    fi
    out="$(timeout 600 cargo test --locked fuzz 2>&1)" || { printf '%s\n' "$out"; exit 1; }
    printf '%s\n' "$out"
    ran="$(printf '%s\n' "$out" | awk '/^test result:/ { for (i = 2; i <= NF; i++) if ($i ~ /^passed/) s += $(i-1) } END { print s + 0 }')"
    if [ "$ran" -eq 0 ]; then
        echo "fuzz: targets detected but 'cargo test --locked fuzz' ran 0 tests — name embedded targets fn fuzz_*" >&2
        exit 1
    fi

# miri on governor-core only — the pure crate; I/O deps (rusqlite, tokio,
# daemon glue) can never run under Miri. Subset tests are named `fn miri_*`
miri:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! rustup toolchain list | grep -q '^nightly'; then
        echo "miri: nightly toolchain with the miri component is required" >&2
        echo "miri: rustup toolchain install nightly --component miri" >&2
        exit 1
    fi
    if ! grep -rq 'fn miri_' governor-core 2>/dev/null; then
        echo "miri: no pure-logic subset tests (fn miri_*) exist yet; skipping"
        exit 0
    fi
    timeout 900 cargo +nightly miri test --locked -p governor-core miri_
