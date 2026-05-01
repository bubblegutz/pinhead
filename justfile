release-upx:
    cargo build --release && upx -9 target/release/ph

install: release-upx
    cp target/release/ph {{env_var("HOME")}}/.cargo/bin/ph

test *args:
    cargo test {{args}} --release -- --nocapture
