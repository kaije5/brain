use std::{fs, path::PathBuf, time::Duration};

use ed25519_dalek::{Signer, SigningKey};
use serde::Deserialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};
use uuid::Uuid;

use crate::CommandRequest;

const MAX_FRAME_BYTES: usize = 64 * 1024;
const STREAMING_REQUEST_TIMEOUT: Duration = Duration::from_mins(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("daemon discovery is unavailable")]
    DiscoveryUnavailable,
    #[error("local enrollment is unavailable")]
    EnrollmentUnavailable,
    #[error("invalid command input")]
    InvalidInput,
    #[error("local daemon is unavailable")]
    TransportUnavailable,
    #[error("local daemon request timed out")]
    Timeout,
    #[error("daemon request failed: {0}")]
    Daemon(String),
}

impl ClientError {
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::DiscoveryUnavailable => "discovery_unavailable",
            Self::EnrollmentUnavailable => "enrollment_unavailable",
            Self::InvalidInput => "invalid_input",
            Self::TransportUnavailable => "transport_unavailable",
            Self::Timeout => "timeout",
            Self::Daemon(code) => code,
        }
    }
}

#[derive(Clone)]
pub struct DaemonClient {
    endpoint_name: String,
    principal_id: Uuid,
    signer: SigningKey,
}

impl DaemonClient {
    #[cfg(test)]
    pub(crate) fn for_keyboard_tests() -> Self {
        Self {
            endpoint_name: "unused-keyboard-test-endpoint".to_owned(),
            principal_id: Uuid::nil(),
            signer: SigningKey::from_bytes(&[0; 32]),
        }
    }

    /// Discovers the local daemon and reads only its per-user enrollment artifact.
    ///
    /// # Errors
    ///
    /// Returns a redacted category when discovery or protected enrollment cannot be read.
    pub fn from_environment() -> Result<Self, ClientError> {
        let database = std::env::var_os("CORTEX_DATABASE")
            .map_or_else(cortexd::default_database_path, PathBuf::from);
        Self::from_database_path(&database)
    }

    /// Builds the client from one database location's discovery and pairing
    /// artifacts, without touching process environment state.
    ///
    /// # Errors
    ///
    /// Returns a redacted category when discovery or enrollment cannot be read.
    pub fn from_database_path(database: &std::path::Path) -> Result<Self, ClientError> {
        let discovery_path = database.with_extension("cortexd-discovery.json");
        let pairing_path = discovery_path.with_extension("cortexd-pairing");
        let discovery: Discovery = serde_json::from_slice(
            &fs::read(discovery_path).map_err(|_| ClientError::DiscoveryUnavailable)?,
        )
        .map_err(|_| ClientError::DiscoveryUnavailable)?;
        let bytes: [u8; 32] = fs::read(pairing_path)
            .map_err(|_| ClientError::EnrollmentUnavailable)?
            .try_into()
            .map_err(|_| ClientError::EnrollmentUnavailable)?;
        Ok(Self {
            endpoint_name: discovery.endpoint_name,
            principal_id: discovery.principal_id,
            signer: SigningKey::from_bytes(&bytes),
        })
    }

    /// Completes one challenge-authenticated bounded request over local IPC.
    ///
    /// # Errors
    ///
    /// Returns a redacted local transport, timeout, or daemon error category.
    pub async fn request(
        &self,
        command: CommandRequest,
    ) -> Result<cortexd::DaemonResponse, ClientError> {
        timeout(REQUEST_TIMEOUT, self.request_inner(command))
            .await
            .map_err(|_| ClientError::Timeout)?
    }

    async fn request_inner(
        &self,
        command: CommandRequest,
    ) -> Result<cortexd::DaemonResponse, ClientError> {
        #[cfg(windows)]
        {
            let stream = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(format!(r"\\.\pipe\{}", self.endpoint_name))
                .map_err(|_| ClientError::TransportUnavailable)?;
            self.request_stream(stream, command).await
        }
        #[cfg(unix)]
        {
            let path = std::env::temp_dir().join(format!("{}.sock", self.endpoint_name));
            let stream = tokio::net::UnixStream::connect(path)
                .await
                .map_err(|_| ClientError::TransportUnavailable)?;
            self.request_stream(stream, command).await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = command;
            Err(ClientError::TransportUnavailable)
        }
    }

    /// Streaming form of [`Self::request`]: intermediate `partial` frames are
    /// reported through `on_partial` as they arrive on the same authenticated
    /// session; the returned response is the terminal frame.
    ///
    /// # Errors
    /// Returns a redacted local transport, timeout, or daemon error category.
    pub async fn request_streaming(
        &self,
        command: CommandRequest,
        on_partial: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<cortexd::DaemonResponse, ClientError> {
        timeout(
            STREAMING_REQUEST_TIMEOUT,
            self.request_streaming_connected(command, on_partial),
        )
        .await
        .map_err(|_| ClientError::Timeout)?
    }

    async fn request_streaming_connected(
        &self,
        command: CommandRequest,
        on_partial: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<cortexd::DaemonResponse, ClientError> {
        #[cfg(windows)]
        {
            let stream = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(format!(r"\\.\pipe\{}", self.endpoint_name))
                .map_err(|_| ClientError::TransportUnavailable)?;
            self.request_streaming_frames(stream, command, on_partial)
                .await
        }
        #[cfg(unix)]
        {
            let path = std::env::temp_dir().join(format!("{}.sock", self.endpoint_name));
            let stream = tokio::net::UnixStream::connect(path)
                .await
                .map_err(|_| ClientError::TransportUnavailable)?;
            self.request_streaming_frames(stream, command, on_partial)
                .await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (command, on_partial);
            Err(ClientError::TransportUnavailable)
        }
    }

    async fn request_streaming_frames<S>(
        &self,
        mut stream: S,
        command: CommandRequest,
        on_partial: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<cortexd::DaemonResponse, ClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let challenge: cortexd::PairingChallenge =
            serde_json::from_slice(&read_frame(&mut stream).await?)
                .map_err(|_| ClientError::TransportUnavailable)?;
        if challenge.protocol_version != cortexd::PROTOCOL_VERSION {
            return Err(ClientError::TransportUnavailable);
        }
        let mut message = b"cortexd-local-ipc-pairing-v1\0".to_vec();
        message.extend_from_slice(&challenge.protocol_version.to_le_bytes());
        message.extend_from_slice(challenge.nonce.as_bytes());
        let proof = cortexd::PairingResponse {
            protocol_version: cortexd::PROTOCOL_VERSION,
            signature: self.signer.sign(&message).to_bytes().to_vec(),
        };
        write_frame(&mut stream, &proof).await?;
        let request = command.into_daemon_request(self.principal_id);
        let expected = request.request_id;
        write_frame(&mut stream, &request).await?;
        loop {
            let response: cortexd::DaemonResponse =
                serde_json::from_slice(&read_frame(&mut stream).await?)
                    .map_err(|_| ClientError::TransportUnavailable)?;
            if response.protocol_version != cortexd::PROTOCOL_VERSION
                || response.request_id != expected
            {
                return Err(ClientError::TransportUnavailable);
            }
            if let cortexd::WireResult::Success { value } = &response.result
                && let Some(partial) = value.get("partial").and_then(serde_json::Value::as_str)
            {
                on_partial(partial);
                continue;
            }
            return Ok(response);
        }
    }

    async fn request_stream<S>(
        &self,
        mut stream: S,
        command: CommandRequest,
    ) -> Result<cortexd::DaemonResponse, ClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let challenge: cortexd::PairingChallenge =
            serde_json::from_slice(&read_frame(&mut stream).await?)
                .map_err(|_| ClientError::TransportUnavailable)?;
        if challenge.protocol_version != cortexd::PROTOCOL_VERSION {
            return Err(ClientError::TransportUnavailable);
        }
        let mut message = b"cortexd-local-ipc-pairing-v1\0".to_vec();
        message.extend_from_slice(&challenge.protocol_version.to_le_bytes());
        message.extend_from_slice(challenge.nonce.as_bytes());
        let proof = cortexd::PairingResponse {
            protocol_version: cortexd::PROTOCOL_VERSION,
            signature: self.signer.sign(&message).to_bytes().to_vec(),
        };
        write_frame(&mut stream, &proof).await?;
        let request = command.into_daemon_request(self.principal_id);
        let expected = request.request_id;
        write_frame(&mut stream, &request).await?;
        let response: cortexd::DaemonResponse =
            serde_json::from_slice(&read_frame(&mut stream).await?)
                .map_err(|_| ClientError::TransportUnavailable)?;
        if response.protocol_version != cortexd::PROTOCOL_VERSION || response.request_id != expected
        {
            return Err(ClientError::TransportUnavailable);
        }
        Ok(response)
    }
}

#[derive(Deserialize)]
struct Discovery {
    endpoint_name: String,
    principal_id: Uuid,
}

async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, ClientError> {
    let size = usize::try_from(
        stream
            .read_u32_le()
            .await
            .map_err(|_| ClientError::TransportUnavailable)?,
    )
    .map_err(|_| ClientError::TransportUnavailable)?;
    if size == 0 || size > MAX_FRAME_BYTES {
        return Err(ClientError::TransportUnavailable);
    }
    let mut bytes = vec![0; size];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| ClientError::TransportUnavailable)?;
    Ok(bytes)
}
async fn write_frame<S: AsyncWrite + Unpin, T: serde::Serialize>(
    stream: &mut S,
    value: &T,
) -> Result<(), ClientError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ClientError::TransportUnavailable)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(ClientError::TransportUnavailable);
    }
    stream
        .write_u32_le(u32::try_from(bytes.len()).map_err(|_| ClientError::TransportUnavailable)?)
        .await
        .map_err(|_| ClientError::TransportUnavailable)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|_| ClientError::TransportUnavailable)?;
    stream
        .flush()
        .await
        .map_err(|_| ClientError::TransportUnavailable)
}
