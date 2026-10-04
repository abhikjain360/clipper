use std::{path::Path, process::Command as ProcessCommand};

use chrono::DateTime;
use clipper_cli::{Command, Error, RangeArgs, ScheduleCommand};
use clipper_daemon_client::Connection;
use clipper_daemon_types::{
    ActualView, ApiErrorCode, AppState, AuthChallenge, AuthenticateResult, AuthenticatedSession,
    DaemonCommand, DaemonEvent, DaemonRequest, DaemonResponse, ErrorResponse, IPC_AUTH_NONCE_BYTES,
    IPC_AUTH_VERSION, OccurrenceView, ScheduleItemView, ipc_client_auth_message,
    ipc_daemon_auth_message,
};
use clipper_schedule::{
    BlockDuration, Expansion, Recurrence, RecurrenceEngine, ScheduleItem, ScheduleItemId,
    ScheduleSpan, TimeRange, TimedStart,
};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
};
use uuid::Uuid;
use zeroize::Zeroizing;

const SECRET: [u8; 32] = [7; 32];

async fn execute(path: &Path, command: Command, input: &[u8]) -> Result<Value, Error> {
    let mut connection =
        Connection::connect_with_secret(path, || Ok(Zeroizing::new(SECRET.to_vec()))).await?;
    let mut output = Vec::new();
    command.execute(&mut connection, input, &mut output).await?;
    Ok(serde_json::from_slice(&output).unwrap())
}

fn schedule(command: ScheduleCommand) -> Command {
    Command::Schedule { command }
}

fn range() -> RangeArgs {
    RangeArgs {
        from: "2026-10-12".into(),
        to: "2026-10-13".into(),
    }
}

#[tokio::test]
async fn schedule_changes_use_authenticated_connections_and_reject_stale_writes() {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let daemon = tokio::spawn(serve(listener));
        let mut item = ScheduleItem {
            break_reminders: false,
            id: ScheduleItemId::new(),
            title: "Planning".into(),
            span: ScheduleSpan::Timed {
                start: TimedStart::Zoned {
                    local: "2026-10-12T09:00:00".parse().unwrap(),
                    zone: chrono_tz::Europe::Berlin,
                },
                duration: BlockDuration::from_minutes(45).unwrap(),
            },
            recurrence: Recurrence::Once,
            reference: None,
            alarm: None,
        };
        let mut draft = serde_json::to_value(&item).unwrap();
        draft.as_object_mut().unwrap().remove("id");
        let saved = execute(
            &path,
            schedule(ScheduleCommand::Add),
            &serde_json::to_vec(&draft).unwrap(),
        )
        .await
        .unwrap();
        let object_id: Uuid = saved["object_id"].as_str().unwrap().parse().unwrap();
        assert_eq!(saved, serde_json::json!({"object_id": object_id}));
        let items = execute(&path, schedule(ScheduleCommand::Items), b"")
            .await
            .unwrap();
        assert_eq!(items.as_array().unwrap().len(), 1);
        item = serde_json::from_value(items[0]["item"].clone()).unwrap();
        let series_id = item.id;
        item.title = "Updated planning".into();
        let input = serde_json::to_vec(&item).unwrap();
        let stale = execute(
            &path,
            schedule(ScheduleCommand::Update {
                object_id,
                revision: 0,
            }),
            &input,
        )
        .await;
        let Err(Error::Daemon(clipper_daemon_client::ClientError::Daemon(error))) = stale else {
            panic!("daemon rejects the stale write");
        };
        assert_eq!(
            error.message,
            "This schedule changed since the editor opened; reopen it before saving"
        );
        let saved = execute(
            &path,
            schedule(ScheduleCommand::Update {
                object_id,
                revision: items[0]["revision"].as_u64().unwrap(),
            }),
            &input,
        )
        .await
        .unwrap();
        assert_eq!(saved, serde_json::json!({"object_id": object_id}));
        let occurrences = execute(
            &path,
            schedule(ScheduleCommand::Occurrences {
                range: range(),
                zone: Some(chrono_tz::Europe::Berlin),
            }),
            b"",
        )
        .await
        .unwrap();
        assert_eq!(occurrences.as_array().unwrap().len(), 1);
        assert_eq!(occurrences[0]["title"], "Updated planning");
        assert_eq!(occurrences[0]["item_id"], series_id.to_string());
        let actuals = execute(&path, Command::Actuals(range()), b"")
            .await
            .unwrap();
        assert_eq!(actuals.as_array().unwrap().len(), 1);
        execute(&path, schedule(ScheduleCommand::Delete { object_id }), b"")
            .await
            .unwrap();
        let items = execute(&path, schedule(ScheduleCommand::Items), b"")
            .await
            .unwrap();
        assert!(items.as_array().unwrap().is_empty());
        let signed_out = execute(&path, schedule(ScheduleCommand::Items), b"").await;
        assert!(matches!(signed_out, Err(Error::NotLoggedIn)));
        let signed_out = execute(&path, Command::Actuals(range()), b"").await;
        let Err(Error::Daemon(clipper_daemon_client::ClientError::Daemon(error))) = signed_out
        else {
            panic!("daemon rejects the signed-out request");
        };
        assert_eq!(error.message, "Not logged in");
        let unverified =
            Connection::connect_with_secret(&path, || Ok(Zeroizing::new(SECRET.to_vec()))).await;
        assert!(matches!(
            unverified,
            Err(clipper_daemon_client::ClientError::Protocol(_))
        ));
        let binary = env!("CARGO_BIN_EXE_clipper");
        let stale_path = path.clone();
        let stale = tokio::task::spawn_blocking(move || {
            ProcessCommand::new(binary)
                .args(["schedule", "items"])
                .env("CLIPPER_DAEMON_SOCKET_PATH", stale_path)
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(!stale.status.success());
        assert!(stale.stdout.is_empty());
        assert!(
            String::from_utf8(stale.stderr)
                .unwrap()
                .contains("restart the Clipper daemon")
        );
        daemon.await.unwrap();
        let absent = ProcessCommand::new(binary)
            .args(["schedule", "items"])
            .env(
                "CLIPPER_DAEMON_SOCKET_PATH",
                directory.path().join("absent.sock"),
            )
            .output()
            .unwrap();
        assert!(!absent.status.success());
        assert!(absent.stdout.is_empty());
        assert!(
            String::from_utf8(absent.stderr)
                .unwrap()
                .contains("open Clipper")
        );
    })
    .await
    .expect("temporary daemon completes");
}

async fn serve(listener: UnixListener) {
    let object_id = Uuid::new_v4().to_string();
    let mut stored: Option<ScheduleItem> = None;
    let mut revision = 0;
    for session in 0..12 {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let nonce = vec![3; IPC_AUTH_NONCE_BYTES];
        let challenge = DaemonEvent::auth_challenge(AuthChallenge {
            protocol_version: if session == 11 {
                IPC_AUTH_VERSION - 1
            } else {
                IPC_AUTH_VERSION
            },
            daemon_nonce: nonce.clone(),
        });
        write
            .write_all(format!("{}\n", serde_json::to_string(&challenge).unwrap()).as_bytes())
            .await
            .unwrap();
        if session == 11 {
            let mut line = String::new();
            assert_eq!(reader.read_line(&mut line).await.unwrap(), 0);
            continue;
        }
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let request: DaemonRequest = serde_json::from_str(&line).unwrap();
        let DaemonCommand::Authenticate(auth) = request.command else {
            panic!("authentication required");
        };
        assert_eq!(auth.protocol_version, IPC_AUTH_VERSION);
        let mut mac = Hmac::<Sha256>::new_from_slice(&SECRET).unwrap();
        mac.update(&ipc_client_auth_message(&nonce, &auth.client_nonce));
        mac.verify_slice(&auth.tag).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(&SECRET).unwrap();
        mac.update(&ipc_daemon_auth_message(&nonce, &auth.client_nonce));
        let mut tag = mac.finalize().into_bytes().to_vec();
        if session == 10 {
            tag[0] ^= 1;
        }
        let response = DaemonResponse::success(
            request.id,
            Some(
                serde_json::to_value(AuthenticateResult {
                    protocol_version: IPC_AUTH_VERSION,
                    tag,
                })
                .unwrap(),
            ),
        );
        write
            .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
            .await
            .unwrap();
        if session == 10 {
            line.clear();
            assert_eq!(reader.read_line(&mut line).await.unwrap(), 0);
            continue;
        }
        let event = DaemonEvent::state_changed(AppState::default());
        write
            .write_all(format!("{}\n", serde_json::to_string(&event).unwrap()).as_bytes())
            .await
            .unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let request: DaemonRequest = serde_json::from_str(&line).unwrap();
        let result = match request.command {
            DaemonCommand::CreateScheduleItem(params) => {
                assert!(stored.is_none());
                stored = Some(params.item);
                revision = 1;
                Ok(Some(serde_json::to_value(&object_id).unwrap()))
            }
            DaemonCommand::UpdateScheduleItem(params) => {
                assert_eq!(params.object_id, object_id);
                let previous = stored.as_ref().unwrap();
                if params.expected_revision != revision {
                    Err(ErrorResponse::new(
                        ApiErrorCode::ValidationFailed,
                        "This schedule changed since the editor opened; reopen it before saving",
                    ))
                } else {
                    assert_eq!(previous.id, params.item.id);
                    stored = Some(params.item);
                    revision += 1;
                    Ok(Some(serde_json::to_value(&object_id).unwrap()))
                }
            }
            DaemonCommand::GetState => {
                let schedule_items = stored
                    .iter()
                    .map(|item| ScheduleItemView {
                        id: object_id.clone(),
                        revision,
                        definition_json: serde_json::to_string(item).unwrap(),
                        ..Default::default()
                    })
                    .collect();
                Ok(Some(
                    serde_json::to_value(AppState {
                        session: (session != 8).then(AuthenticatedSession::default),
                        schedule_items,
                        ..Default::default()
                    })
                    .unwrap(),
                ))
            }
            DaemonCommand::ExpandSchedule(params) => {
                let item = stored.as_ref().unwrap();
                let from = DateTime::parse_from_rfc3339(&params.from).unwrap().to_utc();
                let to = DateTime::parse_from_rfc3339(&params.to).unwrap().to_utc();
                let occurrences = RecurrenceEngine::new()
                    .occurrences(
                        item,
                        &[],
                        &Expansion {
                            window: TimeRange::new(from, to).unwrap(),
                            observer: params.observer_zone.parse().unwrap(),
                        },
                    )
                    .unwrap();
                let occurrences: Vec<_> = occurrences
                    .into_iter()
                    .map(|occurrence| OccurrenceView {
                        item_id: occurrence.item.to_string(),
                        title: item.title.clone(),
                        start: occurrence.span.start().to_rfc3339(),
                        end: occurrence.span.end().to_rfc3339(),
                        ..Default::default()
                    })
                    .collect();
                Ok(Some(serde_json::to_value(occurrences).unwrap()))
            }
            DaemonCommand::ActualsBetween(_) if session == 9 => {
                Err(ErrorResponse::new(ApiErrorCode::Unknown, "Not logged in"))
            }
            DaemonCommand::ActualsBetween(params) => {
                let from = DateTime::parse_from_rfc3339(&params.from).unwrap();
                let to = DateTime::parse_from_rfc3339(&params.to).unwrap();
                assert!(to > from);
                Ok(Some(
                    serde_json::to_value(vec![ActualView {
                        id: Uuid::new_v4().to_string(),
                        start: from.to_rfc3339(),
                        end: to.to_rfc3339(),
                        ..Default::default()
                    }])
                    .unwrap(),
                ))
            }
            DaemonCommand::DeleteScheduleObject(params) => {
                assert_eq!(params.object_id, object_id);
                assert!(stored.take().is_some());
                Ok(None)
            }
            _ => panic!("unexpected command"),
        };
        let response = match result {
            Ok(value) => DaemonResponse::success(request.id, value),
            Err(error) => DaemonResponse::error(request.id, error),
        };
        write
            .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
            .await
            .unwrap();
        line.clear();
        assert_eq!(reader.read_line(&mut line).await.unwrap(), 0);
    }
}
