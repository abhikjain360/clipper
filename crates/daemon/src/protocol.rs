//! IPC protocol types — re-exported from clipper-daemon-types.

#[allow(unused_imports)]
pub use clipper_daemon_types::{
    AuthChallenge, AuthenticateParams, ClipboardPayloadParams, ClipboardPayloadResult,
    CopyToLocalParams, CopyToLocalResult, DaemonCommand, DaemonEvent, DaemonEventKind, DaemonLine,
    DaemonRequest, DaemonResponse, DeleteFileParams, DownloadFileParams, IPC_AUTH_NONCE_BYTES,
    IPC_AUTH_TAG_BYTES, IPC_AUTH_VERSION, LoginParams, RegisterParams, RegisterResult,
    SendClipboardParams, SendClipboardPayloadParams, UploadFileParams, UploadFileResult,
    ipc_auth_message,
};
