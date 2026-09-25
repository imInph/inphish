use std::process::Command;

#[test]
fn bench_signature_is_stable() {
    let output = Command::new(env!("CARGO_BIN_EXE_inphish"))
        .args(["bench", "4"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let line = String::from_utf8(output.stdout).unwrap();
    assert_eq!(line.split_whitespace().next(), Some("79837"));
}
