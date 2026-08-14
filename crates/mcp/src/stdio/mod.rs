use std::{
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use rmcp::{
    service::{RoleClient, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, watch},
};
use tokio_util::sync::CancellationToken;

const CHANNEL_CAPACITY: usize = 16;
const MAX_FRAME_BYTES: usize = 16 * 1_024 * 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Termination {
    Running = 0,
    Closed = 1,
    Cancelled = 2,
    TimedOut = 3,
    Dropped = 4,
    EndOfFile = 5,
    OversizedFrame = 6,
    MalformedFrame = 7,
    IoFailure = 8,
    ChildExited = 9,
}

impl Termination {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Closed,
            2 => Self::Cancelled,
            3 => Self::TimedOut,
            4 => Self::Dropped,
            5 => Self::EndOfFile,
            6 => Self::OversizedFrame,
            7 => Self::MalformedFrame,
            8 => Self::IoFailure,
            9 => Self::ChildExited,
            _ => Self::Running,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StdioTransportError {
    #[error("invalid bounded MCP stdio configuration")]
    InvalidConfiguration,
    #[error("bounded MCP stdio process could not be started")]
    SpawnFailed,
    #[error("bounded MCP stdio pipe was unavailable")]
    MissingPipe,
    #[error("bounded MCP stdio message could not be serialized")]
    SerializationFailed,
    #[error("outbound MCP stdio message exceeded its frame budget")]
    OutboundFrameTooLarge,
    #[error("bounded MCP stdio transport is closed")]
    Closed,
}

/// Exact local command configuration. Debug intentionally exposes counts only.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct BoundedCommand {
    executable: PathBuf,
    arguments: Vec<String>,
    working_directory: PathBuf,
}

impl BoundedCommand {
    pub(crate) fn new(
        executable: impl Into<PathBuf>,
        arguments: Vec<String>,
        working_directory: impl Into<PathBuf>,
    ) -> Result<Self, StdioTransportError> {
        let executable = executable.into();
        let working_directory = working_directory.into();
        if !executable.is_absolute()
            || executable.as_os_str().is_empty()
            || !working_directory.is_absolute()
            || arguments
                .iter()
                .any(|argument| argument.is_empty() || argument.contains('\0'))
        {
            return Err(StdioTransportError::InvalidConfiguration);
        }
        Ok(Self {
            executable,
            arguments,
            working_directory,
        })
    }

    fn build(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .args(&self.arguments)
            .current_dir(&self.working_directory)
            .env_clear()
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("TMPDIR", &self.working_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        command
    }

    #[must_use]
    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub(crate) fn arguments(&self) -> &[String] {
        &self.arguments
    }

    #[must_use]
    pub(crate) fn working_directory(&self) -> &Path {
        &self.working_directory
    }
}

impl fmt::Debug for BoundedCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedCommand")
            .field("executable", &"<redacted>")
            .field("argument_count", &self.arguments.len())
            .field("working_directory", &"<redacted>")
            .field("environment", &"LANG,LC_ALL,TMPDIR only")
            .finish()
    }
}

struct SharedState {
    cancellation: CancellationToken,
    termination: AtomicU8,
}

impl SharedState {
    fn terminate(&self, reason: Termination) {
        let _ = self.termination.compare_exchange(
            Termination::Running as u8,
            reason as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.cancellation.cancel();
    }

    fn termination(&self) -> Termination {
        Termination::from_u8(self.termination.load(Ordering::SeqCst))
    }
}

/// Redacted lifecycle evidence kept independently of the transport receiver.
#[derive(Clone)]
pub(crate) struct ProcessProbe {
    process_id: u32,
    shared: Arc<SharedState>,
    reaped: watch::Receiver<bool>,
}

impl ProcessProbe {
    #[must_use]
    pub(crate) const fn process_id(&self) -> u32 {
        self.process_id
    }

    #[must_use]
    pub(crate) fn termination(&self) -> Termination {
        self.shared.termination()
    }

    pub(crate) async fn wait_reaped(&mut self, timeout: Duration) -> bool {
        if *self.reaped.borrow() {
            return true;
        }
        tokio::time::timeout(timeout, async {
            while self.reaped.changed().await.is_ok() {
                if *self.reaped.borrow() {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false)
    }
}

impl fmt::Debug for ProcessProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessProbe")
            .field("process_id", &"<redacted>")
            .field("termination", &self.termination())
            .field("reaped", &*self.reaped.borrow())
            .finish()
    }
}

/// Bounded newline-delimited JSON transport compatible with rmcp's client.
pub(crate) struct BoundedChildTransport {
    outbound: mpsc::Sender<Vec<u8>>,
    inbound: mpsc::Receiver<RxJsonRpcMessage<RoleClient>>,
    shared: Arc<SharedState>,
    reaped: watch::Receiver<bool>,
    max_frame_bytes: usize,
}

impl BoundedChildTransport {
    pub(crate) fn spawn(
        command: &BoundedCommand,
        max_frame_bytes: usize,
    ) -> Result<(Self, ProcessProbe), StdioTransportError> {
        if max_frame_bytes == 0 || max_frame_bytes > MAX_FRAME_BYTES {
            return Err(StdioTransportError::InvalidConfiguration);
        }
        let mut child = command
            .build()
            .spawn()
            .map_err(|_| StdioTransportError::SpawnFailed)?;
        let process_id = child.id().ok_or(StdioTransportError::SpawnFailed)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(StdioTransportError::MissingPipe)?;
        let stdin = child.stdin.take().ok_or(StdioTransportError::MissingPipe)?;
        let shared = Arc::new(SharedState {
            cancellation: CancellationToken::new(),
            termination: AtomicU8::new(Termination::Running as u8),
        });
        let (outbound, outbound_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (inbound_tx, inbound) = mpsc::channel(CHANNEL_CAPACITY);
        let (reaped_tx, reaped) = watch::channel(false);

        tokio::spawn(read_frames(
            stdout,
            max_frame_bytes,
            inbound_tx,
            shared.clone(),
        ));
        tokio::spawn(write_frames(stdin, outbound_rx, shared.clone()));
        tokio::spawn(supervise_child(
            child,
            process_id,
            shared.clone(),
            reaped_tx,
        ));

        let probe = ProcessProbe {
            process_id,
            shared: shared.clone(),
            reaped: reaped.clone(),
        };
        Ok((
            Self {
                outbound,
                inbound,
                shared,
                reaped,
                max_frame_bytes,
            },
            probe,
        ))
    }

    pub(crate) fn cancel(&self) {
        self.shared.terminate(Termination::Cancelled);
    }

    pub(crate) fn terminate_timeout(&self) {
        self.shared.terminate(Termination::TimedOut);
    }

    async fn wait_reaped(&mut self) {
        while !*self.reaped.borrow() && self.reaped.changed().await.is_ok() {}
    }
}

impl Drop for BoundedChildTransport {
    fn drop(&mut self) {
        self.shared.terminate(Termination::Dropped);
    }
}

impl Transport<RoleClient> for BoundedChildTransport {
    type Error = StdioTransportError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let outbound = self.outbound.clone();
        let shared = self.shared.clone();
        let max_frame_bytes = self.max_frame_bytes;
        async move {
            let frame =
                serde_json::to_vec(&item).map_err(|_| StdioTransportError::SerializationFailed)?;
            if frame.len() > max_frame_bytes {
                shared.terminate(Termination::OversizedFrame);
                return Err(StdioTransportError::OutboundFrameTooLarge);
            }
            outbound
                .send(frame)
                .await
                .map_err(|_| StdioTransportError::Closed)
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        self.inbound.recv().await
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.shared.terminate(Termination::Closed);
        self.wait_reaped().await;
        Ok(())
    }
}

async fn read_frames(
    mut stdout: ChildStdout,
    max_frame_bytes: usize,
    inbound: mpsc::Sender<RxJsonRpcMessage<RoleClient>>,
    shared: Arc<SharedState>,
) {
    let mut frame = Vec::with_capacity(max_frame_bytes);
    let mut byte = [0_u8; 1];
    loop {
        let read = tokio::select! {
            () = shared.cancellation.cancelled() => return,
            result = stdout.read(&mut byte) => result,
        };
        match read {
            Ok(0) => {
                shared.terminate(if frame.is_empty() {
                    Termination::EndOfFile
                } else {
                    Termination::MalformedFrame
                });
                return;
            }
            Ok(_) if byte[0] == b'\n' => {
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                if frame.is_empty() {
                    continue;
                }
                let message = match serde_json::from_slice(&frame) {
                    Ok(message) => message,
                    Err(_) => {
                        shared.terminate(Termination::MalformedFrame);
                        return;
                    }
                };
                frame.clear();
                if inbound.send(message).await.is_err() {
                    shared.terminate(Termination::Dropped);
                    return;
                }
            }
            Ok(_) if frame.len() == max_frame_bytes => {
                shared.terminate(Termination::OversizedFrame);
                return;
            }
            Ok(_) => frame.push(byte[0]),
            Err(_) => {
                shared.terminate(Termination::IoFailure);
                return;
            }
        }
    }
}

async fn write_frames(
    mut stdin: ChildStdin,
    mut outbound: mpsc::Receiver<Vec<u8>>,
    shared: Arc<SharedState>,
) {
    loop {
        let frame = tokio::select! {
            () = shared.cancellation.cancelled() => return,
            frame = outbound.recv() => frame,
        };
        let Some(frame) = frame else {
            return;
        };
        if stdin.write_all(&frame).await.is_err()
            || stdin.write_all(b"\n").await.is_err()
            || stdin.flush().await.is_err()
        {
            shared.terminate(Termination::IoFailure);
            return;
        }
    }
}

async fn supervise_child(
    mut child: Child,
    process_id: u32,
    shared: Arc<SharedState>,
    reaped: watch::Sender<bool>,
) {
    tokio::select! {
        _ = shared.cancellation.cancelled() => {
            kill_process_group(process_id);
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        _ = child.wait() => {
            shared.terminate(Termination::ChildExited);
            kill_process_group(process_id);
        }
    }
    let _ = reaped.send(true);
}

fn kill_process_group(process_id: u32) {
    if let Ok(process_group) = i32::try_from(process_id) {
        // SAFETY: the child was spawned as leader of a new process group. A
        // negative pid targets only that group, never the Sotto parent group.
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}
