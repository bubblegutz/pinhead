CARGO_BIN := if env_var("CARGO_HOME") != "" { env_var("CARGO_HOME") } else { env_var("HOME") + "/.cargo" }

release-upx:
    cargo build --release && upx -9 target/release/ph

install: release-upx
    cp target/release/ph {{CARGO_BIN}}/bin/ph

test *args:
    cargo test {{args}} --release -- --nocapture
