use assert_cmd::Command;
use std::{fs, path::Path, process::Command as ProcessCommand};

fn host_triple() -> String {
    zeen_driver::Target::host().triple
}

#[test]
fn targets_list_prints_all_supported_targets() {
    let output = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .arg("--targets-list")
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("Supported targets (14):"));

    for triple in [
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        "i686-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-gnu",
        "x86_64-pc-windows-msvc",
        "i686-pc-windows-gnu",
        "i686-pc-windows-msvc",
        "wasm32-unknown-unknown",
        "wasm32-unknown-wasip1",
        "x86_64-unknown-freebsd",
        "aarch64-linux-android",
    ] {
        assert!(stdout.contains(triple), "missing `{triple}` in the list");
    }
}

#[test]
fn target_has_no_short_flag() {
    let output = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("--target <TRIPLE>"));
    assert!(
        !stdout.contains("-t, --target"),
        "`-t` short flag must not exist"
    );
}

#[test]
fn unsupported_target_is_rejected() {
    Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .args([
            "source.zn",
            "out",
            "--target",
            "powerpc64le-unknown-linux-gnu",
        ])
        .assert()
        .code(1);
}

#[test]
fn explicit_host_target_compiles_and_runs() {
    let triple = host_triple();
    if zeen_linker::linker::ObjectLinker::detect(&triple).is_err() {
        eprintln!("skipping: no host toolchain found for `{triple}`");
        return;
    }

    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_cases/hello_world.zn");
    let binary =
        std::env::temp_dir().join(format!("zeen-cli-host-target-{}.bin", std::process::id()));

    Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .arg(&source)
        .arg(&binary)
        .args(["--target", &host_triple()])
        .assert()
        .success();

    let output = ProcessCommand::new(&binary).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Hello, World!\n");

    let _ = fs::remove_file(&binary);
}
