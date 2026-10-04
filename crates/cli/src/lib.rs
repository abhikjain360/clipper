use std::io::{Read, Write};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc, Weekday};
use chrono_tz::Tz;
use clap::{Args, Parser, Subcommand};
use clipper_daemon_client::{ClientError, Connection};
use clipper_daemon_types::{
    ActualsBetweenParams, AppState, CreateScheduleItemParams, DaemonCommand,
    DeleteScheduleObjectParams, DeviceListResult, ExpandScheduleParams, UpdateScheduleItemParams,
};
use clipper_schedule::{
    AlarmPolicy, BlockDuration, Cadence, Frequency, Recurrence, ScheduleItem, ScheduleItemId,
    ScheduleSpan, TimeRange, TimedStart, WeekdaySet,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "clipper",
    version,
    about = "Read devices and manage the local Clipper daemon's schedule"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(about = "Read and write schedule items")]
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    #[command(about = "Read recorded actual time in a range")]
    Actuals(RangeArgs),
    #[command(about = "List the account's devices, marking this device")]
    Devices,
}

#[derive(Subcommand)]
pub enum ScheduleCommand {
    #[command(about = "List every item with its object id, revision and full definition")]
    Items,
    #[command(about = "Expand occurrences in a range")]
    Occurrences {
        #[command(flatten)]
        range: RangeArgs,
        #[arg(long, help = "IANA observer zone; defaults to the system zone")]
        zone: Option<Tz>,
    },
    #[command(about = "Create one full schedule item from JSON on stdin; generate an id if absent", after_help = example_help())]
    Add,
    #[command(about = "Replace one item with JSON on stdin; retain its existing item id")]
    Update {
        object_id: Uuid,
        #[arg(long, help = "Current revision from schedule items")]
        revision: u64,
    },
    #[command(about = "Delete one schedule object")]
    Delete { object_id: Uuid },
}

#[derive(Args)]
pub struct RangeArgs {
    #[arg(long, help = "Inclusive date, local datetime, or RFC 3339 datetime")]
    pub from: String,
    #[arg(long, help = "Exclusive date, local datetime, or RFC 3339 datetime")]
    pub to: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not logged in; open Clipper and sign in")]
    NotLoggedIn,
    #[error(transparent)]
    Daemon(#[from] ClientError),
    #[error("invalid schedule item JSON: {0}; see clipper schedule add --help")]
    Json(#[from] serde_json::Error),
    #[error("CLI I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot determine the system IANA zone: {0}; for occurrences, pass --zone")]
    SystemZone(#[from] iana_time_zone::GetTimezoneError),
    #[error("unknown system IANA zone: {0}")]
    UnknownZone(String),
    #[error("invalid date or datetime {0:?}; use YYYY-MM-DD, YYYY-MM-DDTHH:MM[:SS], or RFC 3339")]
    Date(String),
    #[error(transparent)]
    Time(#[from] clipper_schedule::TimeError),
    #[error("invalid daemon result: {0}")]
    Result(String),
    #[error("cannot determine the Clipper data directory")]
    DataDirectory,
}

#[derive(Serialize)]
struct Item {
    object_id: String,
    revision: u64,
    item: ScheduleItem,
}

#[derive(Serialize)]
struct Device {
    id: String,
    name: String,
    platform: String,
    is_current: bool,
}

enum Output {
    Items,
    Devices,
    Saved,
    Result,
}

impl Command {
    pub async fn execute(
        self,
        connection: &mut Connection,
        input: impl Read,
        mut output: impl Write,
    ) -> Result<(), Error> {
        let (command, result_kind) = self.request(input)?;
        let result = connection.send(command).await?;
        let result = match result_kind {
            Output::Devices => {
                let result: DeviceListResult = decode_result(result)?;
                serde_json::to_value(
                    result
                        .devices
                        .into_iter()
                        .map(|device| Device {
                            id: device.id,
                            name: device.name,
                            platform: device.platform,
                            is_current: device.is_current,
                        })
                        .collect::<Vec<_>>(),
                )?
            }
            Output::Items => {
                let state: AppState = decode_result(result)?;
                if !state.is_logged_in() {
                    return Err(Error::NotLoggedIn);
                }
                let items = state
                    .schedule_items
                    .into_iter()
                    .map(|view| {
                        Ok(Item {
                            object_id: view.id,
                            revision: view.revision,
                            item: serde_json::from_str(&view.definition_json)
                                .map_err(|error| Error::Result(error.to_string()))?,
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                serde_json::to_value(items)?
            }
            Output::Saved => {
                let object_id: String = decode_result(result)?;
                serde_json::json!({"object_id": object_id})
            }
            Output::Result => result.unwrap_or(Value::Null),
        };
        serde_json::to_writer(&mut output, &result)?;
        output.write_all(b"\n")?;
        output.flush()?;
        Ok(())
    }

    fn request(self, input: impl Read) -> Result<(DaemonCommand, Output), Error> {
        match self {
            Self::Devices => Ok((DaemonCommand::ListDevices, Output::Devices)),
            Self::Schedule {
                command: ScheduleCommand::Items,
            } => Ok((DaemonCommand::GetState, Output::Items)),
            Self::Schedule {
                command: ScheduleCommand::Occurrences { range, zone },
            } => {
                let zone = zone.map(Ok).unwrap_or_else(system_zone)?;
                let range = range.resolve(zone)?;
                Ok((
                    DaemonCommand::ExpandSchedule(ExpandScheduleParams {
                        from: range.start().to_rfc3339(),
                        to: range.end().to_rfc3339(),
                        observer_zone: zone.name().into(),
                    }),
                    Output::Result,
                ))
            }
            Self::Schedule {
                command: ScheduleCommand::Add,
            } => {
                let mut value: Value = serde_json::from_reader(input)?;
                if let Some(fields) = value.as_object_mut() {
                    fields
                        .entry("id")
                        .or_insert_with(|| Value::String(ScheduleItemId::new().to_string()));
                }
                let item = serde_json::from_value(value)?;
                Ok((
                    DaemonCommand::CreateScheduleItem(CreateScheduleItemParams { item }),
                    Output::Saved,
                ))
            }
            Self::Schedule {
                command:
                    ScheduleCommand::Update {
                        object_id,
                        revision,
                    },
            } => {
                let item = serde_json::from_reader(input)?;
                Ok((
                    DaemonCommand::UpdateScheduleItem(UpdateScheduleItemParams {
                        object_id: object_id.to_string(),
                        expected_revision: revision,
                        item,
                    }),
                    Output::Saved,
                ))
            }
            Self::Schedule {
                command: ScheduleCommand::Delete { object_id },
            } => Ok((
                DaemonCommand::DeleteScheduleObject(DeleteScheduleObjectParams {
                    object_id: object_id.to_string(),
                }),
                Output::Result,
            )),
            Self::Actuals(range) => {
                let range = range.resolve(system_zone()?)?;
                Ok((
                    DaemonCommand::ActualsBetween(ActualsBetweenParams {
                        from: range.start().to_rfc3339(),
                        to: range.end().to_rfc3339(),
                    }),
                    Output::Result,
                ))
            }
        }
    }
}

fn decode_result<T: serde::de::DeserializeOwned>(value: Option<Value>) -> Result<T, Error> {
    serde_json::from_value(value.ok_or_else(|| Error::Result("missing result".into()))?)
        .map_err(|error| Error::Result(error.to_string()))
}

fn system_zone() -> Result<Tz, Error> {
    let name = iana_time_zone::get_timezone()?;
    name.parse().map_err(|_| Error::UnknownZone(name))
}

impl RangeArgs {
    fn resolve(self, zone: Tz) -> Result<TimeRange, Error> {
        Ok(TimeRange::new(
            parse_date(&self.from, zone)?,
            parse_date(&self.to, zone)?,
        )?)
    }
}

fn parse_date(value: &str, zone: Tz) -> Result<DateTime<Utc>, Error> {
    if let Ok(instant) = DateTime::parse_from_rfc3339(value) {
        return Ok(instant.with_timezone(&Utc));
    }
    let local = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .or_else(|| {
            [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%dT%H:%M",
                "%Y-%m-%d %H:%M:%S%.f",
                "%Y-%m-%d %H:%M",
            ]
            .into_iter()
            .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
        })
        .ok_or_else(|| Error::Date(value.into()))?;
    Ok(TimedStart::Zoned { local, zone }.resolve(zone)?)
}

fn example_help() -> String {
    let item = ScheduleItem {
        break_reminders: false,
        id: ScheduleItemId(Uuid::from_u128(0x12345678123442348234123456789abc)),
        title: "Morning planning".into(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: NaiveDate::from_ymd_opt(2026, 10, 12)
                    .expect("valid example date")
                    .and_hms_opt(9, 0, 0)
                    .expect("valid example time"),
                zone: chrono_tz::Europe::Berlin,
            },
            duration: BlockDuration::from_minutes(45).expect("positive example duration"),
        },
        recurrence: Recurrence::Every(Cadence::each(Frequency::Weekly {
            weekdays: WeekdaySet::new(&[Weekday::Mon, Weekday::Wed, Weekday::Fri])
                .expect("nonempty example weekdays"),
        })),
        reference: None,
        alarm: Some(AlarmPolicy::minutes_before(5)),
    };
    format!(
        "Example stdin JSON (duration is in minutes; id may be omitted for add):\n{}",
        serde_json::to_string_pretty(&item).expect("example item serializes")
    )
}
