use std::{path::Path, time::Duration};

use cortex_domain::PrincipalId;
use ed25519_dalek::{Signer, SigningKey};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

use crate::{
    DaemonError, DaemonRequest, DaemonResponse, PROTOCOL_VERSION, PairingChallenge,
    PairingResponse, config::load_ipc_enrollment,
};

const MAX_FRAME_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// File-enrolled client for the daemon's authenticated per-user IPC endpoint.
#[derive(Clone)]
pub struct AuthenticatedIpcClient {
    endpoint_name: String,
    principal_id: PrincipalId,
    signer: SigningKey,
}

impl AuthenticatedIpcClient {
    /// Loads the daemon discovery record and its separately protected pairing enrollment.
    ///
    /// # Errors
    /// Returns a redacted configuration error when discovery or enrollment is unavailable or
    /// inconsistent.
    pub fn from_database_path(database_path: &Path) -> Result<Self, DaemonError> {
        let enrollment = load_ipc_enrollment(database_path)?;
        Ok(Self {
            endpoint_name: enrollment.endpoint_name,
            principal_id: enrollment.principal_id,
            signer: enrollment.signer,
        })
    }

    #[must_use]
    pub const fn principal_id(&self) -> PrincipalId {
        self.principal_id
    }

    /// Sends one bounded request after proving possession of the protected enrollment key.
    ///
    /// # Errors
    /// Returns a redacted authentication or local transport category.
    pub async fn request(&self, request: &DaemonRequest) -> Result<DaemonResponse, DaemonError> {
        timeout(REQUEST_TIMEOUT, self.request_inner(request))
            .await
            .map_err(|_| DaemonError::TransportUnavailable)?
    }

    async fn request_inner(&self, request: &DaemonRequest) -> Result<DaemonResponse, DaemonError> {
        #[cfg(windows)]
        {
            let stream = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(format!(r"\\.\pipe\{}", self.endpoint_name))
                .map_err(|_| DaemonError::TransportUnavailable)?;
            self.request_stream(stream, request).await
        }
        #[cfg(unix)]
        {
            let path = std::env::temp_dir().join(format!("{}.sock", self.endpoint_name));
            let stream = tokio::net::UnixStream::connect(path)
                .await
                .map_err(|_| DaemonError::TransportUnavailable)?;
            self.request_stream(stream, request).await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = request;
            Err(DaemonError::TransportUnavailable)
        }
    }

    async fn request_stream<S>(
        &self,
        mut stream: S,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let challenge: PairingChallenge = serde_json::from_slice(&read_frame(&mut stream).await?)
            .map_err(|_| DaemonError::Unauthenticated)?;
        if challenge.protocol_version != PROTOCOL_VERSION {
            return Err(DaemonError::Unauthenticated);
        }
        let proof = PairingResponse {
            protocol_version: PROTOCOL_VERSION,
            signature: self
                .signer
                .sign(&pairing_message(&challenge))
                .to_bytes()
                .to_vec(),
        };
        write_frame(&mut stream, &proof).await?;
        write_frame(&mut stream, request).await?;
        let response: DaemonResponse = serde_json::from_slice(&read_frame(&mut stream).await?)
            .map_err(|_| DaemonError::TransportUnavailable)?;
        if response.protocol_version != PROTOCOL_VERSION
            || response.request_id != request.request_id
        {
            return Err(DaemonError::TransportUnavailable);
        }
        Ok(response)
    }
}

async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, DaemonError> {
    let size = usize::try_from(
        stream
            .read_u32_le()
            .await
            .map_err(|_| DaemonError::TransportUnavailable)?,
    )
    .map_err(|_| DaemonError::TransportUnavailable)?;
    if size == 0 || size > MAX_FRAME_BYTES {
        return Err(DaemonError::TransportUnavailable);
    }
    let mut bytes = vec![0; size];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| DaemonError::TransportUnavailable)?;
    Ok(bytes)
}

async fn write_frame<S: AsyncWrite + Unpin, T: serde::Serialize>(
    stream: &mut S,
    value: &T,
) -> Result<(), DaemonError> {
    let bytes = serde_json::to_vec(value).map_err(|_| DaemonError::TransportUnavailable)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(DaemonError::TransportUnavailable);
    }
    stream
        .write_u32_le(u32::try_from(bytes.len()).map_err(|_| DaemonError::TransportUnavailable)?)
        .await
        .map_err(|_| DaemonError::TransportUnavailable)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|_| DaemonError::TransportUnavailable)?;
    stream
        .flush()
        .await
        .map_err(|_| DaemonError::TransportUnavailable)
}

fn pairing_message(challenge: &PairingChallenge) -> Vec<u8> {
    let mut message = b"cortexd-local-ipc-pairing-v1\0".to_vec();
    message.extend_from_slice(&challenge.protocol_version.to_le_bytes());
    message.extend_from_slice(challenge.nonce.as_bytes());
    message
}
