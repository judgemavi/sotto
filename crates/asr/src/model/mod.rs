//! Verified, resumable provisioning for whisper.cpp model weights.
//!
//! Managed weights are downloaded from an immutable whisper.cpp model revision into
//! `~/Library/Application Support/Sotto/models`. The final filename is never written
//! until its exact byte length and SHA-256 digest have been verified. Interrupted
//! downloads remain under a `.partial` suffix and are resumed with an HTTP range request.

use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, OnceLock, Weak},
};

use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header};
use sha2::{Digest, Sha256};
use sotto_core::CancellationToken;
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex as AsyncMutex,
};

use crate::ModelSize;

/// Immutable whisper.cpp model artifact revision used by the managed provisioner.
pub const MODEL_REVISION: &str = "c521a4b02f422512d734391fdf08bb08c0862f68";
const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve";

type ArtifactMutex = AsyncMutex<()>;
type ArtifactMutexMap = HashMap<PathBuf, Weak<ArtifactMutex>>;

static ARTIFACT_MUTEXES: OnceLock<StdMutex<ArtifactMutexMap>> = OnceLock::new();

/// Immutable source metadata for one supported model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelSpec {
    pub size: ModelSize,
    pub file_name: &'static str,
    pub byte_len: u64,
    pub sha256: &'static str,
}

impl ModelSize {
    /// Returns the pinned artifact metadata for this model size.
    #[must_use]
    pub const fn spec(self) -> ModelSpec {
        match self {
            Self::BaseEn => ModelSpec {
                size: self,
                file_name: "ggml-base.en.bin",
                byte_len: 147_964_211,
                sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
            },
            Self::SmallEn => ModelSpec {
                size: self,
                file_name: "ggml-small.en.bin",
                byte_len: 487_614_201,
                sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
            },
            Self::MediumEn => ModelSpec {
                size: self,
                file_name: "ggml-medium.en.bin",
                byte_len: 1_533_774_781,
                sha256: "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356",
            },
        }
    }
}

/// Current stage and byte counts for first-run UI progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProvisionProgress {
    pub phase: ProvisionPhase,
    pub downloaded: u64,
    pub total: u64,
}

/// Stable phases suitable for a progress label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProvisionPhase {
    Resolving,
    Downloading,
    Verifying,
    Ready,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelProvisionError {
    #[error("could not locate the user's home directory")]
    HomeUnavailable,
    #[error("model provisioning was cancelled at {path}; resumable={resumable}")]
    Cancelled { path: PathBuf, resumable: bool },
    #[error("configured SOTTO_WHISPER_MODEL is not a non-empty file: {path}")]
    InvalidOverride { path: PathBuf },
    #[error("model request failed: {0}")]
    Network(String),
    #[error(
        "No verified Whisper model is available and the download service could not be reached. Connect to the internet and retry, or set SOTTO_WHISPER_MODEL to a local model file"
    )]
    OfflineNoCache,
    #[error("model server returned HTTP {status}")]
    Http { status: u16 },
    #[error("model server returned an invalid range response: {0}")]
    InvalidRange(String),
    #[error("model file operation failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("model {path} has {actual} bytes; expected {expected}")]
    SizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("model {path} has SHA-256 {actual}; expected {expected}")]
    DigestMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
}

/// Resolves cached weights and downloads missing managed weights.
#[derive(Clone, Debug)]
pub struct ModelProvisioner {
    directory: PathBuf,
    client: Client,
    base_url: String,
}

impl ModelProvisioner {
    /// Creates a provisioner rooted at an injectable directory.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            client: Client::new(),
            base_url: MODEL_BASE_URL.to_owned(),
        }
    }

    /// Creates the production provisioner under macOS Application Support.
    pub fn for_current_user() -> Result<Self, ModelProvisionError> {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .ok_or(ModelProvisionError::HomeUnavailable)?;
        Ok(Self::new(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Sotto")
                .join("models"),
        ))
    }

    /// Returns a verified cached model or downloads, verifies, and atomically installs it.
    pub async fn resolve_or_download(
        &self,
        size: ModelSize,
        cancellation: &CancellationToken,
        mut progress: impl FnMut(ProvisionProgress),
    ) -> Result<PathBuf, ModelProvisionError> {
        self.provision(
            Artifact::from(size.spec(), &self.base_url),
            cancellation,
            &mut progress,
        )
        .await
    }

    /// Preserves the development/user-owned `SOTTO_WHISPER_MODEL` override.
    ///
    /// An override is never replaced or downloaded over. Because arbitrary user-owned
    /// weights have no Sotto-controlled digest, this validates that the path is a
    /// non-empty regular file; whisper.cpp remains the authority on model compatibility
    /// when the transcriber loads it.
    pub async fn resolve_configured_or_download(
        &self,
        size: ModelSize,
        cancellation: &CancellationToken,
        progress: impl FnMut(ProvisionProgress),
    ) -> Result<PathBuf, ModelProvisionError> {
        let configured = std::env::var_os("SOTTO_WHISPER_MODEL").map(PathBuf::from);
        self.resolve_with_override(size, configured.as_deref(), cancellation, progress)
            .await
    }

    /// Resolves an explicit override or falls back to managed provisioning.
    pub async fn resolve_with_override(
        &self,
        size: ModelSize,
        override_path: Option<&Path>,
        cancellation: &CancellationToken,
        mut progress: impl FnMut(ProvisionProgress),
    ) -> Result<PathBuf, ModelProvisionError> {
        if let Some(path) = override_path {
            return resolve_override(path, &mut progress).await;
        }
        self.resolve_or_download(size, cancellation, progress).await
    }

    async fn provision(
        &self,
        artifact: Artifact,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(ProvisionProgress),
    ) -> Result<PathBuf, ModelProvisionError> {
        fs::create_dir_all(&self.directory)
            .await
            .map_err(|source| io_error(&self.directory, source))?;
        let lock_directory = fs::canonicalize(&self.directory)
            .await
            .map_err(|source| io_error(&self.directory, source))?;
        let destination = self.directory.join(&artifact.file_name);
        let lock_destination = lock_directory.join(&artifact.file_name);
        let partial = partial_path(&destination);
        progress(ProvisionProgress {
            phase: ProvisionPhase::Resolving,
            downloaded: 0,
            total: artifact.byte_len,
        });

        let artifact_mutex = artifact_mutex(&lock_destination);
        let _artifact_guard = tokio::select! {
            () = cancellation.cancelled() => {
                return Err(cancelled(partial, true));
            }
            guard = artifact_mutex.lock() => guard,
        };
        if cancellation.is_cancelled() {
            return Err(cancelled(partial, true));
        }

        if file_exists(&destination).await? {
            match verify(&destination, &artifact, cancellation, progress).await {
                Ok(()) => {
                    ensure_not_cancelled(cancellation, &destination, false)?;
                    return ready(destination, artifact.byte_len, progress);
                }
                Err(
                    ModelProvisionError::SizeMismatch { .. }
                    | ModelProvisionError::DigestMismatch { .. },
                ) => {
                    let _quarantined = quarantine_corrupt(&destination).await?;
                }
                Err(error) => return Err(error),
            }
        }

        let mut offset = file_len(&partial).await?.unwrap_or(0);
        if offset > artifact.byte_len {
            remove_if_present(&partial).await?;
            offset = 0;
        }
        if offset == artifact.byte_len {
            if let Err(error) = verify(&partial, &artifact, cancellation, progress).await {
                if !matches!(error, ModelProvisionError::Cancelled { .. }) {
                    remove_if_present(&partial).await?;
                }
                return Err(error);
            }
            install(&partial, &destination, cancellation).await?;
            return ready(destination, artifact.byte_len, progress);
        }
        ensure_not_cancelled(cancellation, &partial, true)?;

        let (response, append, response_offset) = self
            .download_response(&artifact, &partial, offset, cancellation)
            .await?;
        offset = response_offset;

        let mut file = open_partial(&partial, append).await?;
        let mut downloaded = offset;
        progress(ProvisionProgress {
            phase: ProvisionPhase::Downloading,
            downloaded,
            total: artifact.byte_len,
        });
        let mut body = response.bytes_stream();
        loop {
            let chunk = tokio::select! {
                () = cancellation.cancelled() => {
                    file.flush().await.map_err(|source| io_error(&partial, source))?;
                    return Err(cancelled(partial, true));
                }
                chunk = body.next() => chunk,
            };
            let Some(chunk) = chunk else {
                break;
            };
            let chunk = chunk.map_err(|error| ModelProvisionError::Network(error.to_string()))?;
            file.write_all(&chunk)
                .await
                .map_err(|source| io_error(&partial, source))?;
            downloaded =
                downloaded.saturating_add(u64::try_from(chunk.len()).map_err(|error| {
                    ModelProvisionError::Network(format!("download chunk is too large: {error}"))
                })?);
            progress(ProvisionProgress {
                phase: ProvisionPhase::Downloading,
                downloaded,
                total: artifact.byte_len,
            });
        }
        file.flush()
            .await
            .map_err(|source| io_error(&partial, source))?;
        file.sync_all()
            .await
            .map_err(|source| io_error(&partial, source))?;
        drop(file);

        if downloaded != artifact.byte_len {
            return Err(ModelProvisionError::SizeMismatch {
                path: partial,
                expected: artifact.byte_len,
                actual: downloaded,
            });
        }
        if let Err(error) = verify(&partial, &artifact, cancellation, progress).await {
            if !matches!(error, ModelProvisionError::Cancelled { .. }) {
                remove_if_present(&partial).await?;
            }
            return Err(error);
        }
        install(&partial, &destination, cancellation).await?;
        ready(destination, artifact.byte_len, progress)
    }

    async fn download_response(
        &self,
        artifact: &Artifact,
        partial: &Path,
        mut offset: u64,
        cancellation: &CancellationToken,
    ) -> Result<(reqwest::Response, bool, u64), ModelProvisionError> {
        loop {
            let mut request = self.client.get(&artifact.url);
            if offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
            }
            let response = tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(cancelled(partial.to_owned(), true));
                }
                response = request.send() => response.map_err(network_or_offline)?,
            };

            let status = response.status();
            if offset == 0 && status == StatusCode::OK {
                return Ok((response, false, 0));
            }
            if offset > 0 && status == StatusCode::PARTIAL_CONTENT {
                match validate_content_range(response.headers(), offset, artifact.byte_len) {
                    Ok(()) => return Ok((response, true, offset)),
                    Err(_) => {
                        remove_if_present(partial).await?;
                        offset = 0;
                        continue;
                    }
                }
            }
            if offset > 0 && status == StatusCode::OK {
                return Ok((response, false, 0));
            }
            if offset > 0 && status == StatusCode::RANGE_NOT_SATISFIABLE {
                remove_if_present(partial).await?;
                offset = 0;
                continue;
            }
            return Err(ModelProvisionError::Http {
                status: status.as_u16(),
            });
        }
    }
}

#[derive(Clone, Debug)]
struct Artifact {
    file_name: String,
    url: String,
    byte_len: u64,
    sha256: String,
}

impl Artifact {
    fn from(spec: ModelSpec, base_url: &str) -> Self {
        Self {
            file_name: spec.file_name.to_owned(),
            url: format!(
                "{}/{MODEL_REVISION}/{}",
                base_url.trim_end_matches('/'),
                spec.file_name
            ),
            byte_len: spec.byte_len,
            sha256: spec.sha256.to_owned(),
        }
    }
}

fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination
        .file_name()
        .map_or_else(OsString::new, OsString::from);
    name.push(".partial");
    destination.with_file_name(name)
}

fn corrupt_path(destination: &Path) -> PathBuf {
    let mut name = destination
        .file_name()
        .map_or_else(OsString::new, OsString::from);
    name.push(".corrupt");
    destination.with_file_name(name)
}

fn artifact_mutex(destination: &Path) -> Arc<ArtifactMutex> {
    let mutexes = ARTIFACT_MUTEXES.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut mutexes = match mutexes.lock() {
        Ok(mutexes) => mutexes,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(mutex) = mutexes.get(destination).and_then(Weak::upgrade) {
        return mutex;
    }
    mutexes.retain(|_, mutex| mutex.strong_count() > 0);
    let mutex = Arc::new(ArtifactMutex::new(()));
    mutexes.insert(destination.to_owned(), Arc::downgrade(&mutex));
    mutex
}

async fn file_exists(path: &Path) -> Result<bool, ModelProvisionError> {
    match fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(path, source)),
    }
}

async fn file_len(path: &Path) -> Result<Option<u64>, ModelProvisionError> {
    match fs::metadata(path).await {
        Ok(metadata) => Ok(Some(metadata.len())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path, source)),
    }
}

async fn open_partial(path: &Path, append: bool) -> Result<File, ModelProvisionError> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .await
        .map_err(|source| io_error(path, source))
}

async fn verify(
    path: &Path,
    artifact: &Artifact,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(ProvisionProgress),
) -> Result<(), ModelProvisionError> {
    let actual_len = file_len(path).await?.unwrap_or(0);
    if actual_len != artifact.byte_len {
        return Err(ModelProvisionError::SizeMismatch {
            path: path.to_owned(),
            expected: artifact.byte_len,
            actual: actual_len,
        });
    }
    progress(ProvisionProgress {
        phase: ProvisionPhase::Verifying,
        downloaded: actual_len,
        total: artifact.byte_len,
    });
    let mut file = File::open(path)
        .await
        .map_err(|source| io_error(path, source))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = tokio::select! {
            () = cancellation.cancelled() => {
                return Err(cancelled(path.to_owned(), path.extension().is_some_and(|value| value == "partial")));
            }
            read = file.read(&mut buffer) => read.map_err(|source| io_error(path, source))?,
        };
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    let actual = hex::encode(hash.finalize());
    if actual != artifact.sha256 {
        return Err(ModelProvisionError::DigestMismatch {
            path: path.to_owned(),
            expected: artifact.sha256.clone(),
            actual,
        });
    }
    Ok(())
}

async fn install(
    partial: &Path,
    destination: &Path,
    cancellation: &CancellationToken,
) -> Result<(), ModelProvisionError> {
    ensure_not_cancelled(cancellation, partial, true)?;
    fs::rename(partial, destination)
        .await
        .map_err(|source| io_error(destination, source))
}

async fn quarantine_corrupt(path: &Path) -> Result<PathBuf, ModelProvisionError> {
    let quarantine = corrupt_path(path);
    remove_if_present(&quarantine).await?;
    fs::rename(path, &quarantine)
        .await
        .map_err(|source| io_error(path, source))?;
    Ok(quarantine)
}

async fn remove_if_present(path: &Path) -> Result<(), ModelProvisionError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(path, source)),
    }
}

fn ready(
    destination: PathBuf,
    total: u64,
    progress: &mut dyn FnMut(ProvisionProgress),
) -> Result<PathBuf, ModelProvisionError> {
    progress(ProvisionProgress {
        phase: ProvisionPhase::Ready,
        downloaded: total,
        total,
    });
    Ok(destination)
}

fn validate_content_range(
    headers: &header::HeaderMap,
    offset: u64,
    expected_total: u64,
) -> Result<(), ModelProvisionError> {
    let value = headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ModelProvisionError::InvalidRange("missing Content-Range".to_owned()))?;
    let value = value
        .strip_prefix("bytes ")
        .ok_or_else(|| ModelProvisionError::InvalidRange("expected bytes unit".to_owned()))?;
    let (range, total) = value
        .split_once('/')
        .ok_or_else(|| ModelProvisionError::InvalidRange("missing range total".to_owned()))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| ModelProvisionError::InvalidRange("missing byte range".to_owned()))?;
    let start = start
        .parse::<u64>()
        .map_err(|_| ModelProvisionError::InvalidRange("invalid range start".to_owned()))?;
    let end = end
        .parse::<u64>()
        .map_err(|_| ModelProvisionError::InvalidRange("invalid range end".to_owned()))?;
    let total = total
        .parse::<u64>()
        .map_err(|_| ModelProvisionError::InvalidRange("invalid range total".to_owned()))?;
    if start != offset || end < start || end >= total || total != expected_total {
        return Err(ModelProvisionError::InvalidRange(format!(
            "expected bytes {offset}-.../{expected_total}, got bytes {start}-{end}/{total}"
        )));
    }
    if let Some(content_length) = headers.get(header::CONTENT_LENGTH) {
        let content_length = content_length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                ModelProvisionError::InvalidRange("invalid Content-Length".to_owned())
            })?;
        let expected_length = end - start + 1;
        if content_length != expected_length {
            return Err(ModelProvisionError::InvalidRange(format!(
                "range contains {expected_length} bytes but Content-Length is {content_length}"
            )));
        }
    }
    Ok(())
}

fn ensure_not_cancelled(
    cancellation: &CancellationToken,
    path: &Path,
    resumable: bool,
) -> Result<(), ModelProvisionError> {
    if cancellation.is_cancelled() {
        Err(cancelled(path.to_owned(), resumable))
    } else {
        Ok(())
    }
}

fn network_or_offline(error: reqwest::Error) -> ModelProvisionError {
    if error.is_connect() || error.is_timeout() {
        ModelProvisionError::OfflineNoCache
    } else {
        ModelProvisionError::Network(error.to_string())
    }
}

fn io_error(path: &Path, source: std::io::Error) -> ModelProvisionError {
    ModelProvisionError::Io {
        path: path.to_owned(),
        source,
    }
}

fn cancelled(path: PathBuf, resumable: bool) -> ModelProvisionError {
    ModelProvisionError::Cancelled { path, resumable }
}

async fn resolve_override(
    path: &Path,
    progress: &mut dyn FnMut(ProvisionProgress),
) -> Result<PathBuf, ModelProvisionError> {
    let metadata = fs::metadata(path)
        .await
        .map_err(|_| ModelProvisionError::InvalidOverride {
            path: path.to_owned(),
        })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(ModelProvisionError::InvalidOverride {
            path: path.to_owned(),
        });
    }
    progress(ProvisionProgress {
        phase: ProvisionPhase::Ready,
        downloaded: metadata.len(),
        total: metadata.len(),
    });
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        path::Path,
        sync::mpsc,
        thread,
        time::Duration,
    };

    use sha2::{Digest, Sha256};
    use sotto_core::CancellationToken;
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    use super::{
        Artifact, ModelProvisionError, ModelProvisioner, ModelSize, ProvisionPhase, corrupt_path,
        install, partial_path,
    };

    #[test]
    fn small_english_is_the_default_with_pinned_metadata() {
        let spec = ModelSize::default().spec();
        // Changed from base.en on 2026-08-14 on measured accuracy, not preference. Moving it again
        // is legitimate, but only with a number: see docs/experiments/asr-model-benchmark.md.
        assert_eq!(
            spec.size,
            ModelSize::SmallEn,
            "small.en must remain the default"
        );
        assert_eq!(
            spec.file_name, "ggml-small.en.bin",
            "default filename must match whisper.cpp"
        );
        assert_eq!(spec.sha256.len(), 64, "SHA-256 must have 64 hex digits");
    }

    #[test]
    fn every_model_size_pins_a_distinct_verifiable_artifact() {
        let specs =
            [ModelSize::BaseEn, ModelSize::SmallEn, ModelSize::MediumEn].map(ModelSize::spec);

        for spec in specs {
            assert_eq!(spec.sha256.len(), 64, "{} lacks a digest", spec.file_name);
            assert!(spec.byte_len > 0, "{} lacks a length", spec.file_name);
        }
        // A default change is a one-line edit, and a digest copied from the wrong row would install
        // a model that passes verification under another size's name.
        for (index, spec) in specs.iter().enumerate() {
            for other in &specs[index + 1..] {
                assert_ne!(spec.sha256, other.sha256, "two sizes share a digest");
                assert_ne!(
                    spec.file_name, other.file_name,
                    "two sizes share a filename"
                );
            }
        }
    }

    #[tokio::test]
    async fn cached_model_is_verified_without_network() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let bytes = b"verified model";
        let artifact = fixture_artifact("http://127.0.0.1:1/model", bytes);
        let path = directory.path().join(&artifact.file_name);
        fs_write(&path, bytes).await?;
        let provisioner = ModelProvisioner::new(directory.path());
        let mut phases = Vec::new();
        let resolved = provisioner
            .provision(artifact, &CancellationToken::new(), &mut |value| {
                phases.push(value.phase);
            })
            .await?;
        assert_eq!(resolved, path, "cached path should be returned");
        assert_eq!(
            phases,
            vec![
                ProvisionPhase::Resolving,
                ProvisionPhase::Verifying,
                ProvisionPhase::Ready
            ],
            "cached resolution should verify without downloading"
        );
        Ok(())
    }

    #[tokio::test]
    async fn corrupted_cached_model_is_quarantined_and_replaced()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let expected = b"expected bytes";
        let (url, request, server) = serve(expected, expected.len(), Duration::ZERO, false)?;
        let artifact = fixture_artifact(&url, expected);
        let path = directory.path().join(&artifact.file_name);
        fs_write(&path, b"corruptd bytes").await?;
        let resolved = ModelProvisioner::new(directory.path())
            .provision(artifact, &CancellationToken::new(), &mut |_| {})
            .await?;
        assert_eq!(
            tokio::fs::read(&resolved).await?,
            expected,
            "same-sized corrupt data must be replaced with the verified artifact"
        );
        assert_eq!(
            tokio::fs::read(corrupt_path(&path)).await?,
            b"corruptd bytes",
            "the rejected managed cache must be quarantined for diagnosis"
        );
        let _request = request.recv_timeout(Duration::from_secs(1))?;
        join_server(server)?;
        Ok(())
    }

    #[tokio::test]
    async fn offline_without_a_cached_model_is_actionable() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        drop(listener);
        let artifact = fixture_artifact(&format!("http://{address}/model"), b"model");
        let result = ModelProvisioner::new(directory.path())
            .provision(artifact, &CancellationToken::new(), &mut |_| {})
            .await;
        let Err(error) = result else {
            return Err("offline provisioning without a cache must fail".into());
        };
        assert!(
            matches!(&error, ModelProvisionError::OfflineNoCache),
            "connection failure with no usable cache must have a typed offline state"
        );
        assert!(
            error.to_string().contains("Connect to the internet")
                && error.to_string().contains("SOTTO_WHISPER_MODEL"),
            "offline error must give both recovery actions"
        );
        Ok(())
    }

    #[tokio::test]
    async fn explicit_override_is_used_without_managed_download()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let override_path = directory.path().join("user-owned.bin");
        fs_write(&override_path, b"user-owned whisper weights").await?;
        let resolved = ModelProvisioner::new(directory.path().join("managed"))
            .resolve_with_override(
                ModelSize::BaseEn,
                Some(&override_path),
                &CancellationToken::new(),
                |_| {},
            )
            .await?;
        assert_eq!(
            resolved, override_path,
            "an explicit override must win over managed provisioning"
        );
        assert!(
            !directory.path().join("managed").exists(),
            "using an override must not create or download managed weights"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_interrupts_cached_digest_verification()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let bytes = vec![7_u8; 2 * 1024 * 1024];
        let artifact = fixture_artifact("http://127.0.0.1:1/model", &bytes);
        let destination = directory.path().join(&artifact.file_name);
        fs_write(&destination, &bytes).await?;
        let cancellation = CancellationToken::new();
        let cancel_signal = cancellation.clone();
        let result = ModelProvisioner::new(directory.path())
            .provision(artifact, &cancellation, &mut |value| {
                if value.phase == ProvisionPhase::Verifying {
                    cancel_signal.cancel();
                }
            })
            .await;
        assert!(
            matches!(
                result,
                Err(ModelProvisionError::Cancelled {
                    resumable: false,
                    ..
                })
            ),
            "cancellation must interrupt hashing without calling the file ready"
        );
        assert!(
            destination.is_file(),
            "cancelling cached verification must not delete user data"
        );
        Ok(())
    }

    #[tokio::test]
    async fn interrupted_download_is_resumable_and_never_installed()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let bytes = b"first chunk-second chunk";
        let (url, request, server) = serve(bytes, 5, Duration::from_millis(100), false)?;
        let artifact = fixture_artifact(&url, bytes);
        let destination = directory.path().join(&artifact.file_name);
        let partial = partial_path(&destination);
        let cancellation = CancellationToken::new();
        let cancel_signal = cancellation.clone();
        let result = ModelProvisioner::new(directory.path())
            .provision(artifact, &cancellation, &mut |value| {
                if value.phase == ProvisionPhase::Downloading && value.downloaded >= 5 {
                    cancel_signal.cancel();
                }
            })
            .await;
        assert!(
            matches!(result, Err(ModelProvisionError::Cancelled { .. })),
            "mid-stream cancellation must be explicit"
        );
        assert!(
            !destination.exists(),
            "partial data must never be installed"
        );
        assert!(partial.is_file(), "partial data should remain resumable");
        let _request = request.recv_timeout(Duration::from_secs(1))?;
        join_server(server)?;
        Ok(())
    }

    #[tokio::test]
    async fn partial_download_resumes_with_a_range_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let bytes = b"resume this model";
        let offset = 7;
        let (url, request, server) =
            serve(&bytes[offset..], bytes.len() - offset, Duration::ZERO, true)?;
        let artifact = fixture_artifact(&url, bytes);
        let destination = directory.path().join(&artifact.file_name);
        fs_write(&partial_path(&destination), &bytes[..offset]).await?;
        let resolved = ModelProvisioner::new(directory.path())
            .provision(artifact, &CancellationToken::new(), &mut |_| {})
            .await?;
        let received = request.recv_timeout(Duration::from_secs(1))?;
        assert!(
            received.contains("Range: bytes=7-") || received.contains("range: bytes=7-"),
            "resume request must identify the existing byte offset"
        );
        assert_eq!(
            tokio::fs::read(&resolved).await?,
            bytes,
            "resumed artifact must match the complete model"
        );
        assert!(
            !partial_path(&destination).exists(),
            "successful install must consume the partial file"
        );
        join_server(server)?;
        Ok(())
    }

    #[tokio::test]
    async fn ignored_range_restarts_from_zero_without_duplication()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let bytes = b"server ignored range";
        let offset = 6;
        let (url, request, server) = serve(bytes, bytes.len(), Duration::ZERO, false)?;
        let artifact = fixture_artifact(&url, bytes);
        let destination = directory.path().join(&artifact.file_name);
        fs_write(&partial_path(&destination), &bytes[..offset]).await?;
        let resolved = ModelProvisioner::new(directory.path())
            .provision(artifact, &CancellationToken::new(), &mut |_| {})
            .await?;
        assert_eq!(
            tokio::fs::read(resolved).await?,
            bytes,
            "a server ignoring Range must replace, not append to, the partial"
        );
        let received = request.recv_timeout(Duration::from_secs(1))?;
        assert!(
            received.contains("Range: bytes=6-") || received.contains("range: bytes=6-"),
            "the ignored response must follow an attempted resume"
        );
        join_server(server)?;
        Ok(())
    }

    #[tokio::test]
    async fn unsatisfiable_range_discards_partial_and_retries_full()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let bytes = b"range retry model";
        let offset = 5;
        let responses = vec![
            ServedResponse::new(
                "HTTP/1.1 416 Range Not Satisfiable",
                format!("Content-Range: bytes */{}\r\n", bytes.len()),
                Vec::new(),
            ),
            ServedResponse::ok(bytes),
        ];
        let (url, requests, server) = serve_responses(responses)?;
        let artifact = fixture_artifact(&url, bytes);
        let destination = directory.path().join(&artifact.file_name);
        fs_write(&partial_path(&destination), &bytes[..offset]).await?;
        let resolved = ModelProvisioner::new(directory.path())
            .provision(artifact, &CancellationToken::new(), &mut |_| {})
            .await?;
        assert_eq!(
            tokio::fs::read(resolved).await?,
            bytes,
            "416 recovery must install the verified full retry"
        );
        let ranged = requests.recv_timeout(Duration::from_secs(1))?;
        let full = requests.recv_timeout(Duration::from_secs(1))?;
        assert!(
            ranged.contains("Range: bytes=5-") || ranged.contains("range: bytes=5-"),
            "the first request must attempt to resume at the partial length"
        );
        assert!(
            !full.contains("Range:") && !full.contains("range:"),
            "retry after 416 must be a full request"
        );
        join_server(server)?;
        Ok(())
    }

    #[tokio::test]
    async fn malformed_content_range_discards_partial_and_retries_full()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let bytes = b"malformed range model";
        let offset = 4;
        let responses = vec![
            ServedResponse::new(
                "HTTP/1.1 206 Partial Content",
                format!("Content-Range: bytes 0-3/{}\r\n", bytes.len()),
                bytes[..4].to_vec(),
            ),
            ServedResponse::ok(bytes),
        ];
        let (url, requests, server) = serve_responses(responses)?;
        let artifact = fixture_artifact(&url, bytes);
        let destination = directory.path().join(&artifact.file_name);
        fs_write(&partial_path(&destination), &bytes[..offset]).await?;
        let resolved = ModelProvisioner::new(directory.path())
            .provision(artifact, &CancellationToken::new(), &mut |_| {})
            .await?;
        assert_eq!(
            tokio::fs::read(resolved).await?,
            bytes,
            "malformed range recovery must install the verified full retry"
        );
        let ranged = requests.recv_timeout(Duration::from_secs(1))?;
        let full = requests.recv_timeout(Duration::from_secs(1))?;
        assert!(
            ranged.contains("Range: bytes=4-") || ranged.contains("range: bytes=4-"),
            "the first request must attempt to resume at the partial length"
        );
        assert!(
            !full.contains("Range:") && !full.contains("range:"),
            "retry after malformed Content-Range must be a full request"
        );
        join_server(server)?;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_artifact_download()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let bytes = b"one shared artifact";
        let (url, requests, server) = serve(bytes, 4, Duration::from_millis(50), false)?;
        let artifact = fixture_artifact(&url, bytes);
        let first = ModelProvisioner::new(directory.path());
        let second = ModelProvisioner::new(directory.path());
        let cancellation = CancellationToken::new();
        let mut first_progress = |_| {};
        let mut second_progress = |_| {};
        let (first_result, second_result) = tokio::join!(
            first.provision(artifact.clone(), &cancellation, &mut first_progress),
            second.provision(artifact, &cancellation, &mut second_progress),
        );
        assert_eq!(
            first_result?, second_result?,
            "concurrent callers must resolve the same installed artifact"
        );
        let _request = requests.recv_timeout(Duration::from_secs(1))?;
        assert!(
            requests.try_recv().is_err(),
            "only the lock owner may issue the artifact download"
        );
        join_server(server)?;
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_immediately_before_install_keeps_verified_partial()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let destination = directory.path().join("model.bin");
        let partial = partial_path(&destination);
        fs_write(&partial, b"verified").await?;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = install(&partial, &destination, &cancellation).await;
        let Err(error) = result else {
            return Err("late cancellation must prevent installation".into());
        };
        assert!(
            matches!(
                error,
                ModelProvisionError::Cancelled {
                    resumable: true,
                    ..
                }
            ),
            "late cancellation must remain explicitly resumable"
        );
        assert!(partial.is_file(), "verified partial must remain resumable");
        assert!(
            !destination.exists(),
            "cancelled artifact must not be ready"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "downloads 148 MB and runs real whisper.cpp inference"]
    async fn official_base_model_downloads_loads_and_transcribes_fixture()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelProvisioner::new(directory.path())
            .resolve_or_download(ModelSize::BaseEn, &CancellationToken::new(), |_| {})
            .await?;
        let context = WhisperContext::new_with_params(
            model.to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )?;
        let mut state = context.create_state()?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/call-01-mic.wav");
        let mut reader = hound::WavReader::open(fixture)?;
        let samples = reader
            .samples::<i16>()
            .map(|sample| sample.map(|value| f32::from(value) / f32::from(i16::MAX)))
            .collect::<Result<Vec<_>, _>>()?;
        state.full(params, &samples)?;
        let transcript = state
            .as_iter()
            .map(|segment| segment.to_str_lossy().map(|text| text.into_owned()))
            .collect::<Result<Vec<_>, _>>()?
            .join(" ")
            .to_ascii_lowercase();
        assert!(
            transcript.contains("pricing") || transcript.contains("enterprise"),
            "real downloaded model must recover a known fixture concept: {transcript}"
        );
        Ok(())
    }

    fn fixture_artifact(url: &str, bytes: &[u8]) -> Artifact {
        Artifact {
            file_name: "fixture-model.bin".to_owned(),
            url: url.to_owned(),
            byte_len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    }

    async fn fs_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
        tokio::fs::write(path, bytes).await
    }

    type Server = (
        String,
        mpsc::Receiver<String>,
        thread::JoinHandle<std::io::Result<()>>,
    );

    struct ServedResponse {
        status: &'static str,
        headers: String,
        body: Vec<u8>,
        first_chunk: usize,
        pause: Duration,
    }

    impl ServedResponse {
        fn new(status: &'static str, headers: String, body: Vec<u8>) -> Self {
            let first_chunk = body.len();
            Self {
                status,
                headers,
                body,
                first_chunk,
                pause: Duration::ZERO,
            }
        }

        fn ok(body: &[u8]) -> Self {
            Self::new("HTTP/1.1 200 OK", String::new(), body.to_vec())
        }
    }

    fn serve(
        bytes: &[u8],
        first_chunk: usize,
        pause: Duration,
        partial_content: bool,
    ) -> Result<Server, Box<dyn std::error::Error>> {
        let status = if partial_content {
            "HTTP/1.1 206 Partial Content"
        } else {
            "HTTP/1.1 200 OK"
        };
        let headers = if partial_content {
            format!("Content-Range: bytes 7-{}/17\r\n", 6 + bytes.len())
        } else {
            String::new()
        };
        serve_responses(vec![ServedResponse {
            status,
            headers,
            body: bytes.to_vec(),
            first_chunk,
            pause,
        }])
    }

    fn serve_responses(
        responses: Vec<ServedResponse>,
    ) -> Result<Server, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let (request_tx, request_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept()?;
                let mut request_bytes = [0_u8; 4096];
                let read = stream.read(&mut request_bytes)?;
                let request = String::from_utf8_lossy(&request_bytes[..read]).into_owned();
                let _ = request_tx.send(request);
                write!(
                    stream,
                    "{}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
                    response.status,
                    response.body.len(),
                    response.headers
                )?;
                let split = response.first_chunk.min(response.body.len());
                if let Err(error) = stream.write_all(&response.body[..split]) {
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                    ) {
                        continue;
                    }
                    return Err(error);
                }
                stream.flush()?;
                thread::sleep(response.pause);
                if let Err(error) = stream.write_all(&response.body[split..])
                    && !matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                    )
                {
                    return Err(error);
                }
            }
            Ok(())
        });
        Ok((format!("http://{address}/model"), request_rx, server))
    }

    fn join_server(
        server: thread::JoinHandle<std::io::Result<()>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        server.join().map_err(|_| "mock model server panicked")??;
        Ok(())
    }
}
