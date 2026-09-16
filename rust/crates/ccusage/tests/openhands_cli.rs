use csusage_test_support::{Fixture, openhands::create_fixture};

#[test]
fn openhands_cli_tables_snapshot_production_stdout_and_stderr() {
    let fixture = Fixture::new();
    let _ = fixture.create_dir_all("openhands");
    create_fixture(fixture.path("openhands"));

    for kind in ["daily", "monthly", "session"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_csusage"))
            .env_clear()
            .env("HOME", fixture.path("home"))
            .env("USERPROFILE", fixture.path("userprofile"))
            .env("XDG_CONFIG_HOME", fixture.path("xdg-config"))
            .env("OPENHANDS_DIR", fixture.path("openhands"))
            .args([
                "openhands",
                kind,
                "--since",
                "20250101",
                "--until",
                "20270101",
                "--mode",
                "calculate",
                "--offline",
                "--no-color",
                "--timezone",
                "UTC",
            ])
            .output()
            .expect("failed to run csusage");

        assert!(
            output.status.success(),
            "csusage openhands {kind} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8(output.stdout).expect("CLI stdout was not UTF-8");
        let stderr = String::from_utf8(output.stderr).expect("CLI stderr was not UTF-8");
        insta::assert_snapshot!(
            format!("openhands_cli_{kind}_table"),
            format!("stdout:\n{stdout}\nstderr:\n{stderr}")
        );
    }
}
