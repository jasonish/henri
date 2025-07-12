default:

fix:
    cargo clippy --fix --allow-dirty
    cargo fmt

check:
    cargo clippy
    cargo msrv verify
    cargo clippy --all-features --all-targets --target x86_64-pc-windows-gnu
