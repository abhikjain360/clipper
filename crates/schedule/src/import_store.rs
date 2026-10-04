use std::io::{Read, Write};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use super::*;

const MAX_DELTA_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct StoredSource {
    id: SourceId,
    name: String,
    kind: SourceKind,
    enabled: bool,
    #[serde(default)]
    owner_email: Option<String>,
    #[serde(default = "alarms_on_by_default")]
    alarms_on: bool,
    #[serde(default = "alarm_lead_by_default")]
    alarm_lead_minutes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_device: Option<DeviceId>,
    active_import: Option<CalendarImport>,
    #[serde(
        default,
        alias = "pending_import",
        deserialize_with = "pending_imports"
    )]
    pending_imports: Vec<CalendarImport>,
    #[serde(default)]
    retired_imports: Vec<RetiredImport>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "write_delta",
        deserialize_with = "read_delta"
    )]
    delta: Option<StoredDelta>,
}

fn write_delta<S: serde::Serializer>(
    delta: &Option<StoredDelta>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let Some(delta) = delta else {
        return serializer.serialize_none();
    };
    let bytes = serde_json::to_vec(delta).map_err(serde::ser::Error::custom)?;
    if bytes.len() > MAX_DELTA_BYTES {
        return Err(serde::ser::Error::custom(
            "Calendar delta exceeds the expanded size limit",
        ));
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&bytes)
        .map_err(serde::ser::Error::custom)?;
    let bytes = encoder.finish().map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&format!("zlib:{}", STANDARD.encode(bytes)))
}

fn read_delta<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<StoredDelta>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Packed(String),
        Plain(Box<StoredDelta>),
    }
    let Some(stored) = Option::<Stored>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match stored {
        Stored::Plain(delta) => Ok(Some(*delta)),
        Stored::Packed(value) => {
            let encoded = value
                .strip_prefix("zlib:")
                .ok_or_else(|| serde::de::Error::custom("Unknown calendar delta encoding"))?;
            let bytes = STANDARD.decode(encoded).map_err(serde::de::Error::custom)?;
            let mut decoder =
                flate2::read::ZlibDecoder::new(bytes.as_slice()).take((MAX_DELTA_BYTES + 1) as u64);
            let mut expanded = Vec::new();
            decoder
                .read_to_end(&mut expanded)
                .map_err(serde::de::Error::custom)?;
            if expanded.len() > MAX_DELTA_BYTES {
                return Err(serde::de::Error::custom(
                    "Calendar delta exceeds the expanded size limit",
                ));
            }
            serde_json::from_slice(&expanded)
                .map(Some)
                .map_err(serde::de::Error::custom)
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredDelta {
    active: Option<CalendarImport>,
    pending: Vec<CalendarImport>,
    retired: Vec<RetiredImport>,
    retained: Vec<CalendarImport>,
    event_ids: BTreeMap<Uuid, ObjectId>,
    #[serde(default)]
    removing: bool,
    #[serde(default)]
    superseded: HashMap<ObjectId, ObjectId>,
    #[serde(default)]
    pending_retirements: HashSet<ObjectId>,
}

impl From<StoredSource> for CalendarSource {
    fn from(stored: StoredSource) -> Self {
        let import_anchor = stored.active_import.as_ref().map(|batch| batch.object_id);
        let delta_state = stored.delta.is_some();
        let delta = stored.delta.unwrap_or(StoredDelta {
            active: stored.active_import,
            pending: stored.pending_imports,
            retired: stored.retired_imports,
            retained: Vec::new(),
            event_ids: BTreeMap::new(),
            removing: false,
            superseded: HashMap::new(),
            pending_retirements: HashSet::new(),
        });
        Self {
            id: stored.id,
            name: stored.name,
            kind: stored.kind,
            enabled: stored.enabled,
            owner_email: stored.owner_email,
            alarms_on: stored.alarms_on,
            alarm_lead_minutes: stored.alarm_lead_minutes,
            target_device: stored.target_device,
            active_import: delta.active,
            pending_imports: delta.pending,
            retired_imports: delta.retired,
            retained_imports: delta.retained,
            event_ids: delta.event_ids,
            import_anchor,
            delta_state,
            removing: delta.removing,
            superseded: delta.superseded,
            pending_retirements: delta.pending_retirements,
        }
    }
}

impl From<CalendarSource> for StoredSource {
    fn from(source: CalendarSource) -> Self {
        let active_import = if source.delta_state {
            source.import_anchor.map(|anchor| {
                let latest = source
                    .active_import
                    .as_ref()
                    .or_else(|| source.pending_imports.last());
                let mut batch = latest.cloned().unwrap_or(CalendarImport {
                    object_id: anchor,
                    fetched_at: chrono::DateTime::UNIX_EPOCH,
                    events: Vec::new(),
                    content_hash: Vec::new(),
                    window: None,
                    uids: Vec::new(),
                    hashes: Vec::new(),
                    removed: Vec::new(),
                });
                batch.object_id = anchor;
                batch.events = source
                    .imports()
                    .flat_map(|batch| batch.events.iter().copied())
                    .collect();
                batch.events.sort_by_key(|id| Uuid::from(*id));
                batch.events.dedup();
                batch.window = None;
                batch.uids.clear();
                batch.hashes.clear();
                batch.removed.clear();
                batch
            })
        } else {
            source.active_import.clone()
        };
        let pending_imports = if source.delta_state {
            Vec::new()
        } else {
            source.pending_imports.clone()
        };
        let retired_imports = if source.delta_state {
            Vec::new()
        } else {
            source.retired_imports.clone()
        };
        let delta = source.delta_state.then_some(StoredDelta {
            active: source.active_import,
            pending: source.pending_imports,
            retired: source.retired_imports,
            retained: source.retained_imports,
            event_ids: source.event_ids,
            removing: source.removing,
            superseded: source.superseded,
            pending_retirements: source.pending_retirements,
        });
        Self {
            id: source.id,
            name: source.name,
            kind: source.kind,
            enabled: source.enabled,
            owner_email: source.owner_email,
            alarms_on: source.alarms_on,
            alarm_lead_minutes: source.alarm_lead_minutes,
            target_device: source.target_device,
            active_import,
            pending_imports,
            retired_imports,
            delta,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> serde_json::Value {
        serde_json::json!({
            "id": Uuid::new_v4(), "name": "Work", "enabled": true,
            "kind": { "protocol": "ics", "url": "https://example.com/calendar.ics" },
            "active_import": null, "pending_imports": [], "retired_imports": [],
        })
    }

    #[test]
    fn calendar_alarm_lead_defaults_and_is_accepted_by_older_readers() {
        #[derive(Deserialize)]
        struct SourceSettings {
            name: String,
            #[serde(default = "alarms_on_by_default")]
            alarms_on: bool,
        }
        let mut saved: CalendarSource = serde_json::from_value(source()).unwrap();
        assert_eq!(saved.alarm_lead_minutes, 5);
        saved.alarm_lead_minutes = 10;
        let value = serde_json::to_value(&saved).unwrap();
        let older: SourceSettings = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(older.name, "Work");
        assert!(older.alarms_on);
        assert_eq!(
            serde_json::from_value::<CalendarSource>(value)
                .unwrap()
                .alarm_lead_minutes,
            10
        );
    }

    #[test]
    fn calendar_manifests_load_packed_and_plain_delta_state() {
        let mut saved: CalendarSource = serde_json::from_value(source()).unwrap();
        saved.delta_state = true;
        saved.import_anchor = Some(Uuid::new_v4().into());
        saved
            .event_ids
            .insert(Uuid::new_v4(), Uuid::new_v4().into());
        let mut packed = serde_json::to_value(&saved).unwrap();
        assert!(packed["delta"].as_str().unwrap().starts_with("zlib:"));
        assert_eq!(
            serde_json::from_value::<CalendarSource>(packed.clone()).unwrap(),
            saved
        );
        let bytes = STANDARD
            .decode(
                packed["delta"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("zlib:")
                    .unwrap(),
            )
            .unwrap();
        let mut expanded = String::new();
        flate2::read::ZlibDecoder::new(bytes.as_slice())
            .read_to_string(&mut expanded)
            .unwrap();
        packed["delta"] = serde_json::from_str(&expanded).unwrap();
        assert_eq!(
            serde_json::from_value::<CalendarSource>(packed).unwrap(),
            saved
        );
    }

    #[test]
    fn calendar_manifest_expansion_is_bounded() {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&vec![b' '; MAX_DELTA_BYTES + 1]).unwrap();
        let mut saved = source();
        saved["delta"] = serde_json::Value::String(format!(
            "zlib:{}",
            STANDARD.encode(encoder.finish().unwrap())
        ));
        let error = serde_json::from_value::<CalendarSource>(saved).unwrap_err();
        assert!(error.to_string().contains("expanded size limit"));
    }
}
