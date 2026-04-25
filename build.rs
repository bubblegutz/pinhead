fn main() {
    // rlua with default features automatically builds LuaJIT from source
    // (via luajit-src) when the "lua" feature is enabled.
    // This build.rs exists to trigger reruns if needed.
    println!("cargo:rerun-if-changed=build.rs");
}
