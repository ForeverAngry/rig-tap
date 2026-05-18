# rig-tap task runner.
#
#   brew install just
#
# Run `just` with no args to see the recipe list.

default:
    @just --list

# Format check + clippy + tests + msrv + doc.
check: fmt clippy test msrv doc

fmt:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets -- -D warnings
    cargo clippy --all-features --all-targets -- -D warnings

test:
    cargo test --all-targets
    cargo test --all-features --all-targets

msrv:
    cargo +1.89 build --all-targets

doc:
    RUSTDOCFLAGS="-D warnings -D rustdoc::broken_intra_doc_links" cargo doc --all-features --no-deps
