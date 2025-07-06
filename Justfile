default:

fix:
    cargo clippy --fix --allow-dirty
    cargo fmt
