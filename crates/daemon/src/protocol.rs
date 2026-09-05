//! IPC protocol types — re-exported from clipper-daemon-types.

#[allow(unused_imports)]
pub use clipper_daemon_types::{
    AddCalendarSourceParams, AuthChallenge, AuthenticateParams, AuthenticateResult,
    ClipboardPayloadParams, ClipboardPayloadResult, CopyToLocalParams, CopyToLocalResult,
    CreateScheduleItemParams, DaemonCommand, DaemonEvent, DaemonLine, DaemonRequest,
    DaemonResponse, DeleteCollabDocParams, DeleteFileParams, DeleteScheduleObjectParams,
    DeviceListResult, DownloadFileParams, ExpandScheduleParams, GetCollabDocMetaParams,
    IPC_AUTH_NONCE_BYTES, IPC_AUTH_TAG_BYTES, IPC_AUTH_VERSION, LoginParams, RegisterParams,
    RegisterResult, RemoveDeviceParams, SendClipboardParams, SendClipboardPayloadParams,
    SyncCalendarSourceParams, UpdateScheduleItemParams, UploadFileParams, UploadFileResult,
    ipc_client_auth_message, ipc_daemon_auth_message,
};
