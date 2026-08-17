#![forbid(unsafe_code)]

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!("gamepulse-m019-demo-{id}-{}", std::process::id()));
        fs::create_dir_all(&path).expect("temporary project directory must be created");
        Self { path }
    }

    fn write_executable(&self, relative_path: &str, contents: &str) {
        let path = self.path.join(relative_path);
        fs::create_dir_all(path.parent().expect("executable parent must exist"))
            .expect("executable parent directory must be created");
        fs::write(&path, contents).expect("executable fixture must be written");
        let mut permissions = fs::metadata(&path)
            .expect("executable fixture metadata must be readable")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .expect("executable fixture must be marked executable");
    }

    fn prepare_demo_script(&self) {
        self.write_executable("scripts/demo.sh", include_str!("../../../scripts/demo.sh"));
        fs::create_dir_all(self.path.join("tmp")).expect("temporary fixture root must be created");
    }

    fn command(&self, lsof_status: &str, cargo_record: &Path, binary_record: &Path) -> Command {
        let bin = self.path.join("bin");
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let mut command = Command::new("/bin/bash");
        command
            .arg(self.path.join("scripts/demo.sh"))
            .env("PATH", path)
            .env("TMPDIR", self.path.join("tmp"))
            .env("DEMO_LSOF_STATUS", lsof_status)
            .env("DEMO_CARGO_RECORD", cargo_record)
            .env("DEMO_BINARY_RECORD", binary_record);
        command
    }

    fn demo_fixture_dirs(&self) -> Vec<PathBuf> {
        fs::read_dir(self.path.join("tmp"))
            .expect("temporary fixture root must be readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("gamepulse-demo."))
            })
            .collect()
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_test_commands(project: &TempProject) {
    project.write_executable("bin/lsof", "#!/bin/sh\nexit \"${DEMO_LSOF_STATUS:?}\"\n");
    project.write_executable(
        "bin/cargo",
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"${DEMO_CARGO_RECORD:?}\"\nif [ -n \"${GAMEPULSE_M019_FIXTURE_PATH:-}\" ]; then\n  : > \"${GAMEPULSE_M019_FIXTURE_PATH}\"\nfi\nexit 0\n",
    );
    project.write_executable("bin/curl", "#!/bin/sh\nexit 1\n");
}

#[test]
fn demo_task_pins_the_source_disabled_release_contract() {
    let mise = include_str!("../../../mise.toml");
    let script = include_str!("../../../scripts/demo.sh");

    assert!(mise.contains("[tasks.demo]"));
    assert!(mise.contains("run = \"bash scripts/demo.sh\""));
    assert!(script.contains("cargo build --release --locked --offline -p gamepulse"));
    assert!(script.contains("GAMEPULSE_M019_FIXTURE_PATH=\"${demo_database}\""));
    assert!(script.contains("GAMEPULSE_SOURCE_WORK_ENABLED=\"false\""));
    assert!(script.contains("GAMEPULSE_HTTP_ADDRESS=\"${demo_address}\""));
    assert!(script.contains("http://${demo_address}"));
}

#[test]
fn demo_leaves_an_occupied_port_untouched_before_fixture_setup() {
    let project = TempProject::new();
    project.prepare_demo_script();
    write_test_commands(&project);
    let cargo_record = project.path.join("cargo-record");
    let binary_record = project.path.join("binary-record");

    let output = project
        .command("0", &cargo_record, &binary_record)
        .output()
        .expect("demo preflight must run");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("127.0.0.1:3000 is already occupied"));
    assert!(
        !cargo_record.exists(),
        "port preflight must run before fixture setup"
    );
    assert!(project.demo_fixture_dirs().is_empty());
}

#[test]
fn demo_reports_a_failed_local_start_and_cleans_its_fixture() {
    let project = TempProject::new();
    project.prepare_demo_script();
    write_test_commands(&project);
    project.write_executable(
        "target/release/gamepulse",
        "#!/bin/sh\nprintf 'source=%s\\naddress=%s\\n' \"${GAMEPULSE_SOURCE_WORK_ENABLED:?}\" \"${GAMEPULSE_HTTP_ADDRESS:?}\" > \"${DEMO_BINARY_RECORD:?}\"\nexit 1\n",
    );
    let cargo_record = project.path.join("cargo-record");
    let binary_record = project.path.join("binary-record");

    let output = project
        .command("1", &cargo_record, &binary_record)
        .output()
        .expect("demo startup must run");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("source-disabled release UI failed before readiness")
    );
    assert_eq!(
        fs::read_to_string(&binary_record).expect("release fixture must record its environment"),
        "source=false\naddress=127.0.0.1:3000\n"
    );
    let cargo_invocations = fs::read_to_string(&cargo_record).expect("fixture commands must run");
    assert!(cargo_invocations.contains("build --release --locked --offline -p gamepulse"));
    assert!(cargo_invocations.contains("seeds_deterministic_visual_fixture_at_requested_path"));
    assert!(
        project.demo_fixture_dirs().is_empty(),
        "failed startup must clean fixture data"
    );
}
