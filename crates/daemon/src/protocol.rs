//! IPC protocol types — re-exported from clipper-daemon-types.

#[allow(unused_imports)]
pub use clipper_daemon_types::{
    CopyToLocalParams, CopyToLocalResult, DaemonCommand, DaemonEvent, DaemonEventKind, DaemonLine,
    DaemonRequest, DaemonResponse, DeleteFileParams, DownloadFileParams, LoginParams,
    RegisterParams, RegisterResult, SendClipboardParams, UploadFileParams, UploadFileResult,
};
