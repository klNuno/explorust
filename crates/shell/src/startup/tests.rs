use super::*;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

const MODE: &str = "EXPLORUST_TEST_STARTUP_MODE";
const FIXTURE: &str = "EXPLORUST_TEST_STARTUP_FIXTURE";
const TEST: &str = "startup::tests::session_runner_survives_its_launcher";

struct Fixture {
    id: u32,
    dir: PathBuf,
}

impl Fixture {
    fn new(id: u32) -> Self {
        Self {
            id,
            dir: std::env::temp_dir().join(format!("explorust-startup-test-{id}")),
        }
    }

    fn key(&self, name: &str) -> String {
        format!(r"ExplorustStartupTest{}{name}", self.id)
    }

    fn reg(&self, args: &[&str]) -> String {
        let output = Command::new("reg.exe").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "reg {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn test_process(&self, mode: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST])
            .env(MODE, mode)
            .env(FIXTURE, self.id.to_string())
            .stdout(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn wait_for(&self, name: &str) {
        let path = self.dir.join(name);
        let started = Instant::now();
        while !path.exists() && started.elapsed() < Duration::from_secs(15) {
            thread::sleep(Duration::from_millis(25));
        }
        assert!(path.exists(), "missing child marker: {}", path.display());
    }
}

#[test]
fn session_runner_survives_its_launcher() {
    match std::env::var(MODE).as_deref() {
        Ok("launch") => {
            let fixture = Fixture::new(std::env::var(FIXTURE).unwrap().parse().unwrap());
            let mut runner = fixture.test_process("run");
            let _ = runner.wait();
        }
        Ok("run") => {
            let fixture = Fixture::new(std::env::var(FIXTURE).unwrap().parse().unwrap());
            let mut batch = Batch::default();
            launch_registry(
                &mut batch,
                HKEY_CURRENT_USER,
                &fixture.key("RunOnce"),
                registry::NATIVE,
                true,
                false,
                None,
            );
            fixture.wait_for("continue");
            launch_registry(
                &mut batch,
                HKEY_CURRENT_USER,
                &fixture.key("Run"),
                registry::NATIVE,
                false,
                false,
                None,
            );
            batch.finish();
            std::fs::write(fixture.dir.join("finished"), "runner finished").unwrap();
        }
        _ => verify_runner(&Fixture::new(std::process::id())),
    }
}

fn verify_runner(fixture: &Fixture) {
    let base = format!(r"HKCU\{}", registry::version_path(&fixture.key("")));
    let once = format!("{base}RunOnce");
    let run = format!("{base}Run");
    std::fs::create_dir_all(&fixture.dir).unwrap();
    let script = fixture.dir.join("marker.vbs");
    std::fs::write(
        &script,
        format!(
            r#"Set fs = CreateObject("Scripting.FileSystemObject")
kind = WScript.Arguments(0)
fs.CreateTextFile("{}\" & kind, True).Close
If kind = "deferred" Or kind = "rewrite" Then
    For attempt = 1 To 600
        If fs.FileExists("{}\finish-apps") Then Exit For
        WScript.Sleep 25
    Next
End If
If kind = "rewrite" Then
    CreateObject("WScript.Shell").RegWrite "{}!Rewrite", "updated-command", "REG_SZ"
End If
"#,
            fixture.dir.display(),
            fixture.dir.display(),
            format_args!(r"{once}\"),
        ),
    )
    .unwrap();
    for (key, value, kind) in [
        (&once, "!Deferred", "deferred"),
        (&once, "!Rewrite", "rewrite"),
        (&run, "Next", "next"),
    ] {
        let command = format!(r#"wscript.exe //B "{}" {kind}"#, script.display());
        fixture.reg(&[
            "add", key, "/v", value, "/t", "REG_SZ", "/d", &command, "/f",
        ]);
    }
    let mut launcher = fixture.test_process("launch");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture.wait_for("deferred");
        fixture.wait_for("rewrite");
        launcher.kill().unwrap();
        let _ = launcher.wait();
        std::fs::write(fixture.dir.join("continue"), "launcher exited").unwrap();
        fixture.wait_for("next");
        std::fs::write(fixture.dir.join("finish-apps"), "apps may finish").unwrap();
        fixture.wait_for("finished");
        let values = fixture.reg(&["query", &once]);
        assert!(
            !values.contains("!Deferred"),
            "unchanged RunOnce value was left behind"
        );
        assert!(
            values.contains("!Rewrite") && values.contains("updated-command"),
            "replacement was lost: {values}"
        );
        println!(
            "child markers deferred, rewrite and next present after launcher exit; only the exact deferred snapshot was consumed"
        );
    }));
    let _ = launcher.kill();
    let _ = launcher.wait();
    if result.is_err() {
        let _ = std::fs::write(fixture.dir.join("continue"), "test cleanup");
        let _ = std::fs::write(fixture.dir.join("finish-apps"), "test cleanup");
    }
    let _ = Command::new("reg.exe")
        .args(["delete", base.trim_end_matches('\\'), "/f"])
        .output();
    let _ = std::fs::remove_dir_all(&fixture.dir);
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
