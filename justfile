RUSTFLAGS := ""

release-upx:
    RUSTFLAGS='-C link-arg=-static' cargo build --release && upx -9 target/release/ph

install: release-upx
    cp target/release/ph {{env_var("HOME")}}/.cargo/bin/ph

test *args:
    RUSTFLAGS='-C link-arg=-static' cargo test {{args}} --release -- --nocapture

build-static:
    RUSTFLAGS='-C link-arg=-static' cargo build --release
