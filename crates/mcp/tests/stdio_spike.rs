#![deny(warnings)]
#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test compiles only under cfg(test)"
)]

#[path = "../src/stdio/mod.rs"]
mod stdio;

use std::{error::Error, path::Path, time::Duration};

use rmcp::{
    ServiceExt,
    model::{ClientInfo, ReadResourceRequestParams, ReadResourceResponse, ResourceContents},
    transport::Transport,
};
use serde_json::Value;
use stdio::{BoundedChildTransport, BoundedCommand, ProcessProbe, Termination};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

const FRAME_LIMIT: usize = 8 * 1_024;
const WAIT_LIMIT: Duration = Duration::from_secs(3);
const RESOURCE_URI: &str = "docs://fixture/known";

fn fixture_command(mode: &str, directory: &Path) -> Result<BoundedCommand, Box<dyn Error>> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("stdio_child.py");
    Ok(BoundedCommand::new(
        "/Library/Frameworks/Python.framework/Versions/Current/bin/python3",
        vec![fixture.to_string_lossy().into_owned(), mode.to_owned()],
        directory,
    )?)
}

async fn wait_for_pid(directory: &Path) -> Result<i32, Box<dyn Error>> {
    let path = directory.join("descendant.pid");
    tokio::time::timeout(WAIT_LIMIT, async {
        loop {
            if let Ok(contents) = tokio::fs::read_to_string(&path).await
                && let Ok(pid) = contents.parse::<i32>()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(Into::into)
}

async fn assert_tree_reaped(
    mut probe: ProcessProbe,
    descendant_pid: i32,
) -> Result<Termination, Box<dyn Error>> {
    assert!(
        probe.wait_reaped(WAIT_LIMIT).await,
        "direct child must be reaped"
    );
    let termination = probe.termination();
    tokio::time::timeout(WAIT_LIMIT, async {
        while process_exists(descendant_pid)
            || process_exists(i32::try_from(probe.process_id()).unwrap_or(i32::MAX))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(termination)
}

fn process_exists(process_id: i32) -> bool {
    // SAFETY: signal zero performs existence/permission checking and does not
    // deliver a signal. Test pids come directly from spawned fixture processes.
    let result = unsafe { libc::kill(process_id, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

async fn read_audit(directory: &Path) -> Result<Value, Box<dyn Error>> {
    let path = directory.join("audit.json");
    let bytes = tokio::time::timeout(WAIT_LIMIT, async {
        loop {
            if let Ok(bytes) = tokio::fs::read(&path).await {
                return bytes;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::test]
async fn bounded_transport_lists_and_reads_with_exact_process_policy() -> TestResult {
    let directory = TempDir::new()?;
    let command = fixture_command("normal", directory.path())?;
    let expected_arguments = command.arguments().to_vec();
    let expected_executable = command.executable().to_owned();
    let expected_directory = command.working_directory().to_owned();
    let expected_physical_directory = expected_directory.canonicalize()?;
    let (transport, probe) = BoundedChildTransport::spawn(&command, FRAME_LIMIT)?;
    let descendant_pid = wait_for_pid(directory.path()).await?;
    let service = ClientInfo::default().serve(transport).await?;

    let catalog = service.list_resources(None).await?;
    assert_eq!(catalog.resources.len(), 1);
    assert_eq!(catalog.resources[0].uri, RESOURCE_URI);
    let response = service
        .read_resource_once(ReadResourceRequestParams::new(RESOURCE_URI))
        .await?;
    let ReadResourceResponse::Complete(resource) = response else {
        return Err("fixture resource unexpectedly required input".into());
    };
    assert!(matches!(
        resource.contents.as_slice(),
        [ResourceContents::TextResourceContents { text, .. }]
            if text == "bounded fixture resource"
    ));
    service.cancel().await?;

    let termination = assert_tree_reaped(probe, descendant_pid).await?;
    assert_eq!(termination, Termination::Closed);
    let audit = read_audit(directory.path()).await?;
    assert_eq!(
        expected_executable,
        Path::new("/Library/Frameworks/Python.framework/Versions/Current/bin/python3")
    );
    assert_eq!(expected_directory, directory.path());
    assert_eq!(
        audit["argv"],
        serde_json::json!([expected_arguments[0], expected_arguments[1]])
    );
    assert_eq!(
        audit["cwd"],
        expected_physical_directory.to_string_lossy().as_ref()
    );
    assert_eq!(
        audit["environment_keys"],
        serde_json::json!(["LANG", "LC_ALL", "TMPDIR", "__CF_USER_TEXT_ENCODING"]),
        "child environment must contain only the configured allowlist plus macOS' runtime injection"
    );
    let methods = tokio::fs::read_to_string(directory.path().join("methods.txt")).await?;
    assert_eq!(methods.matches("resources/list").count(), 1);
    assert_eq!(methods.matches("resources/read").count(), 1);
    assert!(!methods.contains("tools/"));
    Ok(())
}

#[tokio::test]
async fn input_required_is_observable_without_follow_up_or_tool_call() -> TestResult {
    let directory = TempDir::new()?;
    let command = fixture_command("input-required", directory.path())?;
    let (transport, probe) = BoundedChildTransport::spawn(&command, FRAME_LIMIT)?;
    let descendant_pid = wait_for_pid(directory.path()).await?;
    let service = ClientInfo::default().serve(transport).await?;
    let response = service
        .read_resource_once(ReadResourceRequestParams::new(RESOURCE_URI))
        .await?;
    assert!(matches!(response, ReadResourceResponse::InputRequired(_)));
    service.cancel().await?;

    assert_eq!(
        assert_tree_reaped(probe, descendant_pid).await?,
        Termination::Closed
    );
    let methods = tokio::fs::read_to_string(directory.path().join("methods.txt")).await?;
    assert_eq!(methods.matches("resources/read").count(), 1);
    assert!(!methods.contains("tools/"));
    Ok(())
}

#[tokio::test]
async fn newline_free_oversized_frame_is_bounded_and_reaps_tree() -> TestResult {
    let directory = TempDir::new()?;
    let command = fixture_command("oversized", directory.path())?;
    let (mut transport, probe) = BoundedChildTransport::spawn(&command, 128)?;
    let descendant_pid = wait_for_pid(directory.path()).await?;

    let received = tokio::time::timeout(WAIT_LIMIT, transport.receive()).await?;
    assert!(received.is_none());
    assert_eq!(
        assert_tree_reaped(probe, descendant_pid).await?,
        Termination::OversizedFrame
    );
    Ok(())
}

#[tokio::test]
async fn malformed_frame_reaps_tree_without_echoing_content() -> TestResult {
    let directory = TempDir::new()?;
    let command = fixture_command("malformed", directory.path())?;
    let (mut transport, probe) = BoundedChildTransport::spawn(&command, FRAME_LIMIT)?;
    let descendant_pid = wait_for_pid(directory.path()).await?;

    let received = tokio::time::timeout(WAIT_LIMIT, transport.receive()).await?;
    assert!(received.is_none());
    assert_eq!(
        assert_tree_reaped(probe, descendant_pid).await?,
        Termination::MalformedFrame
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_timeout_and_receiver_drop_each_reap_tree() -> TestResult {
    for (case, expected) in [
        ("cancel", Termination::Cancelled),
        ("timeout", Termination::TimedOut),
        ("drop", Termination::Dropped),
    ] {
        let directory = TempDir::new()?;
        let command = fixture_command("wait", directory.path())?;
        let (mut transport, probe) = BoundedChildTransport::spawn(&command, FRAME_LIMIT)?;
        let descendant_pid = wait_for_pid(directory.path()).await?;
        match case {
            "cancel" => transport.cancel(),
            "timeout" => {
                if tokio::time::timeout(Duration::from_millis(20), transport.receive())
                    .await
                    .is_ok()
                {
                    return Err("silent fixture returned before timeout".into());
                }
                transport.terminate_timeout();
            }
            _ => drop(transport),
        }
        assert_eq!(assert_tree_reaped(probe, descendant_pid).await?, expected);
    }
    Ok(())
}

#[tokio::test]
async fn descendant_that_creates_a_new_session_escapes_process_group_cleanup() -> TestResult {
    let directory = TempDir::new()?;
    let command = fixture_command("escape", directory.path())?;
    let (transport, mut probe) = BoundedChildTransport::spawn(&command, FRAME_LIMIT)?;
    let descendant_pid = wait_for_pid(directory.path()).await?;
    transport.cancel();
    assert!(probe.wait_reaped(WAIT_LIMIT).await);

    let escaped_cleanup = process_exists(descendant_pid);
    // SAFETY: this cleans up the exact fixture pid after recording whether it
    // escaped the transport's process group. It is never a production target.
    unsafe {
        libc::kill(descendant_pid, libc::SIGKILL);
    }
    tokio::time::timeout(WAIT_LIMIT, async {
        while process_exists(descendant_pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert!(
        escaped_cleanup,
        "the spike must preserve evidence that process groups cannot contain a child calling setsid"
    );
    Ok(())
}

#[test]
fn command_and_probe_debug_redact_paths_arguments_and_content() -> TestResult {
    const CANARY: &str = "server-secret-debug-canary";
    let command = BoundedCommand::new(
        format!("/tmp/{CANARY}"),
        vec![CANARY.to_owned()],
        format!("/tmp/{CANARY}-cwd"),
    )?;
    assert!(!format!("{command:?}").contains(CANARY));
    assert!(!format!("{:?}", stdio::StdioTransportError::SpawnFailed).contains(CANARY));
    Ok(())
}
