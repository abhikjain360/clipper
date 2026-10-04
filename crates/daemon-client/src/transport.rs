use std::path::{Path, PathBuf};

use clipper_daemon_types::{
    AuthChallenge, AuthenticateParams, AuthenticateResult, DaemonCommand, DaemonEvent, DaemonLine,
    DaemonRequest, DaemonResponse, ErrorResponse, IPC_AUTH_NONCE_BYTES, IPC_AUTH_TAG_BYTES,
    IPC_AUTH_VERSION, ipc_client_auth_message, ipc_daemon_auth_message,
};
use hmac::{Hmac, Mac};
use rand::RngExt;
use sha2::Sha256;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    time::{Duration, timeout},
};
use zeroize::Zeroizing;

use crate::ipc_secret::{IpcSecretError, load_ipc_secret};

type HmacSha256 = Hmac<Sha256>;
const MAX_LINE: usize = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect to daemon at {}: {source}; open Clipper to start the daemon", path.display())]
    Connect {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "stale daemon protocol version {0}; client requires {IPC_AUTH_VERSION}; restart the Clipper daemon"
    )]
    Version(u32),
    #[error(transparent)]
    Secret(#[from] IpcSecretError),
    #[error("IPC protocol error: {0}")]
    Protocol(String),
    #[error("IPC I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("daemon error: {}", .0.message)]
    Daemon(ErrorResponse),
    #[error("daemon {0} timed out; check Clipper before retrying a write")]
    Timeout(&'static str),
}

pub struct Connection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Connection {
    pub async fn connect(path: &Path, data_dir: &Path) -> Result<Self, ClientError> {
        Self::connect_with_secret(path, || load_ipc_secret(data_dir)).await
    }

    pub async fn connect_with_secret(
        path: &Path,
        load_secret: impl FnOnce() -> Result<Zeroizing<Vec<u8>>, IpcSecretError>,
    ) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|source| ClientError::Connect {
                path: path.to_owned(),
                source,
            })?;
        let (reader, writer) = stream.into_split();
        let mut connection = Self {
            reader: BufReader::new(reader),
            writer,
        };
        timeout(
            Duration::from_secs(10),
            authenticate_with_secret(&mut connection.reader, &mut connection.writer, load_secret),
        )
        .await
        .map_err(|_| ClientError::Timeout("authentication"))??;
        Ok(connection)
    }

    pub async fn send(
        &mut self,
        command: DaemonCommand,
    ) -> Result<Option<serde_json::Value>, ClientError> {
        timeout(Duration::from_secs(60), self.send_request(command))
            .await
            .map_err(|_| ClientError::Timeout("request"))?
    }

    async fn send_request(
        &mut self,
        command: DaemonCommand,
    ) -> Result<Option<serde_json::Value>, ClientError> {
        let request = DaemonRequest::new("1".into(), command);
        write_line(
            &mut self.writer,
            &serde_json::to_string(&request).map_err(protocol_error)?,
        )
        .await?;
        loop {
            let line = read_line(&mut self.reader, &mut Vec::new())
                .await
                .map_err(ClientError::Protocol)?;
            match serde_json::from_str::<DaemonLine>(&line).map_err(protocol_error)? {
                DaemonLine::Response(DaemonResponse::Success { id, result })
                    if id == request.id =>
                {
                    return Ok(result);
                }
                DaemonLine::Response(DaemonResponse::Error { id, error }) if id == request.id => {
                    return Err(ClientError::Daemon(error));
                }
                DaemonLine::Event(DaemonEvent::StateChanged { .. }) => {}
                _ => return Err(ClientError::Protocol("unexpected daemon message".into())),
            }
        }
    }
}

pub async fn authenticate(
    reader: &mut BufReader<OwnedReadHalf>,
    writer: &mut OwnedWriteHalf,
    data_dir: &Path,
) -> Result<(), ClientError> {
    authenticate_with_secret(reader, writer, || load_ipc_secret(data_dir)).await
}

async fn authenticate_with_secret(
    reader: &mut BufReader<OwnedReadHalf>,
    writer: &mut OwnedWriteHalf,
    load_secret: impl FnOnce() -> Result<Zeroizing<Vec<u8>>, IpcSecretError>,
) -> Result<(), ClientError> {
    let line = read_line(reader, &mut Vec::new())
        .await
        .map_err(ClientError::Protocol)?;
    let DaemonEvent::AuthChallenge {
        auth_challenge:
            AuthChallenge {
                protocol_version,
                daemon_nonce,
            },
    } = serde_json::from_str::<DaemonEvent>(&line).map_err(protocol_error)?
    else {
        return Err(ClientError::Protocol("expected AuthChallenge".into()));
    };
    check_version(protocol_version)?;
    if daemon_nonce.len() != IPC_AUTH_NONCE_BYTES {
        return Err(ClientError::Protocol("daemon nonce wrong length".into()));
    }
    let secret = load_secret()?;
    let mut client_nonce = [0u8; IPC_AUTH_NONCE_BYTES];
    rand::rng().fill(&mut client_nonce);
    let tag = hmac_tag(
        &secret,
        &ipc_client_auth_message(&daemon_nonce, &client_nonce),
    )?;
    let request = DaemonRequest::new(
        "auth".into(),
        DaemonCommand::Authenticate(AuthenticateParams {
            protocol_version: IPC_AUTH_VERSION,
            client_nonce: client_nonce.to_vec(),
            tag,
        }),
    );
    write_line(
        writer,
        &serde_json::to_string(&request).map_err(protocol_error)?,
    )
    .await?;
    let line = read_line(reader, &mut Vec::new())
        .await
        .map_err(ClientError::Protocol)?;
    match serde_json::from_str::<DaemonResponse>(&line).map_err(protocol_error)? {
        DaemonResponse::Success {
            id,
            result: Some(value),
        } if id == request.id => {
            let result: AuthenticateResult =
                serde_json::from_value(value).map_err(protocol_error)?;
            check_version(result.protocol_version)?;
            if result.tag.len() != IPC_AUTH_TAG_BYTES {
                return Err(ClientError::Protocol("daemon auth tag wrong length".into()));
            }
            let mut mac = HmacSha256::new_from_slice(&secret).map_err(protocol_error)?;
            mac.update(&ipc_daemon_auth_message(&daemon_nonce, &client_nonce));
            mac.verify_slice(&result.tag)
                .map_err(|_| ClientError::Protocol("daemon HMAC verification failed".into()))?;
            Ok(())
        }
        DaemonResponse::Error { error, .. } => Err(ClientError::Daemon(error)),
        _ => Err(ClientError::Protocol(
            "expected authentication result".into(),
        )),
    }
}

fn check_version(version: u32) -> Result<(), ClientError> {
    if version != IPC_AUTH_VERSION {
        return Err(ClientError::Version(version));
    }
    Ok(())
}

fn protocol_error(error: impl std::fmt::Display) -> ClientError {
    ClientError::Protocol(error.to_string())
}

fn hmac_tag(secret: &[u8], message: &[u8]) -> Result<Vec<u8>, ClientError> {
    let mut mac = HmacSha256::new_from_slice(secret).map_err(protocol_error)?;
    mac.update(message);
    Ok(mac.finalize().into_bytes().to_vec())
}

pub async fn read_line(
    reader: &mut BufReader<OwnedReadHalf>,
    buf: &mut Vec<u8>,
) -> Result<String, String> {
    loop {
        let available = reader.fill_buf().await.map_err(|e| format!("read: {e}"))?;
        if available.is_empty() {
            return Err("daemon disconnected".into());
        }
        let take = available
            .iter()
            .position(|&b| b == b'\n')
            .map_or(available.len(), |p| p + 1);
        if buf.len() + take > MAX_LINE {
            return Err("line too long".into());
        }
        buf.extend_from_slice(&available[..take]);
        reader.consume(take);
        if buf.ends_with(b"\n") {
            break;
        }
    }
    String::from_utf8(std::mem::take(buf))
        .map(|s| s.trim().to_owned())
        .map_err(|e| format!("utf8: {e}"))
}

pub async fn write_line(writer: &mut OwnedWriteHalf, line: &str) -> std::io::Result<()> {
    writer.write_all(format!("{line}\n").as_bytes()).await
}
