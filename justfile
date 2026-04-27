release-upx:
    cargo build --release && upx -9 target/release/pinhead
