use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_elan-haptune"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn help_and_version_do_not_need_a_device() {
    for args in [vec!["--help"], vec!["--version"], vec!["set", "--help"]] {
        let output = cli(&args);
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn invalid_commands_have_json_errors_and_exit_two_before_device_access() {
    for args in [
        vec!["--json", "set"],
        vec!["set", "--json", "--dry-run"],
        vec!["set", "--json", "--press-threshold", "65536"],
        vec!["set", "--json", "--release-threshold", "-1"],
        vec!["set", "--json", "--haptics", "maybe"],
        vec!["set", "--json", "--haptic-level", "1"],
        vec![
            "set",
            "--json",
            "--press-threshold",
            "120",
            "--press-threshold",
            "121",
        ],
        vec!["set", "--json", "--force"],
        vec!["list", "--json", "--device", "/dev/null"],
    ] {
        let output = cli(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["exit_code"], 2);
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn thresholds_without_policy_bounds_reach_device_selection() {
    // Missing selector guarantees these CLI checks cannot issue hardware reports.
    for (press, release, drag) in [
        ("115", "90", "90"),
        ("1", "0", "0"),
        ("65535", "65534", "65534"),
    ] {
        let output = cli(&[
            "set",
            "--json",
            "--dry-run",
            "--device",
            "/dev/elan-haptune-test-nonexistent",
            "--press-threshold",
            press,
            "--release-threshold",
            release,
            "--drag-release-threshold",
            drag,
        ]);
        assert_eq!(output.status.code(), Some(3), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["kind"], "no_device");
    }
}

#[test]
fn nonexistent_explicit_path_is_no_match() {
    let output = cli(&[
        "get",
        "--json",
        "--device",
        "/dev/elan-haptune-test-nonexistent",
    ]);
    assert_eq!(output.status.code(), Some(3));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "no_device");
}

#[test]
fn list_is_sysfs_only_and_json_contains_support_boundaries() {
    let output = cli(&["list", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["command"], "list");
    for d in value["data"]["devices"].as_array().unwrap() {
        assert!(
            d["selector"]
                .as_str()
                .unwrap()
                .starts_with("sysfs:/devices/")
        );
        assert!(
            ["unsupported", "identity_check_required"].contains(&d["support"].as_str().unwrap())
        );
    }
    assert!(output.stderr.is_empty());
}
