use super::*;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delta: Option<StoredDelta>,
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
            target_device: source.target_device,
            active_import,
            pending_imports,
            retired_imports,
            delta,
        }
    }
}
