use csusage_test_support::{Fixture, dsh::create_fixture};

#[test]
fn dsh_cli_tables_snapshot_production_stdout_and_stderr() {
    let fixture = Fixture::new();
    let _ = fixture.create_dir_all("dsh");
    create_fixture(fixture.path("dsh"));

    for kind in ["daily", "monthly", "session"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_csusage"))
            .env_clear()
            .env("HOME", fixture.path("home"))
            .env("USERPROFILE", fixture.path("userprofile"))
            .env("XDG_CONFIG_HOME", fixture.path("xdg-config"))
            .env("DSH_HOME", fixture.path("dsh"))
            .args([
                "dsh",
                kind,
                "--since",
                "19700101",
                "--until",
                "21000101",
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
            "csusage dsh {kind} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8(output.stdout).expect("CLI stdout was not UTF-8");
        let stderr = String::from_utf8(output.stderr).expect("CLI stderr was not UTF-8");
        insta::assert_snapshot!(
            format!("dsh_cli_{kind}_table"),
            format!("stdout:\n{stdout}\nstderr:\n{stderr}")
        );
    }
}
