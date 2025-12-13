import? '../Justfile.common'

default:

check:
    cargo fmt --check
    cargo clippy --all-targets --all-features

fix *args:
    cargo clippy --all-targets --all-features --fix --allow-dirty {{args}}
    cargo fmt

pre-commit: check
    cargo msrv verify

build-release:
    cargo build --target x86_64-unknown-linux-musl --release

install:
    cargo install --path . --target x86_64-unknown-linux-musl
