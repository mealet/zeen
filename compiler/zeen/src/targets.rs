pub const SUPPORTED_TARGETS: &[&str] = &[
    // Linux
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "i686-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    // Mac
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    // Win
    "x86_64-pc-windows-gnu",
    "x86_64-pc-windows-msvc",
    "i686-pc-windows-gnu",
    "i686-pc-windows-msvc",
    // Wasm
    "wasm32-unknown-unknown",
    "wasm32-unknown-wasip1",
    // Others
    "x86_64-unknown-freebsd",
    "aarch64-linux-android",
];
