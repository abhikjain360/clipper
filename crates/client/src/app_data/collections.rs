use clipper_gym::{
    BodyWeight, ConflictRule, Exercise, Recovery, Session, Set, ValidationError, WorkoutTemplate,
};
use clipper_kitchen::{CookingSession, Equipment, KitchenValue, PantryItem, Plan, Recipe};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

type RowIdFromValue = fn(&Value) -> Result<Uuid, String>;
type MergeValues = fn(&Value, &Value) -> Result<Option<Value>, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Storage {
    Rows(ConflictRule),
    Documents,
}

pub(crate) struct Collection {
    pub name: &'static str,
    pub schema_version: u64,
    pub storage: Storage,
    pub indexed_fields: &'static [&'static str],
    pub checked_value: fn(Value) -> Result<Value, String>,
    pub fixed_row_id: Option<RowIdFromValue>,
    pub merge: Option<MergeValues>,
}

impl Collection {
    pub fn schema(&self) -> &'static str {
        self.name
            .split_once('.')
            .map_or(self.name, |(schema, _)| schema)
    }

    pub fn table(&self) -> &'static str {
        self.name
            .split_once('.')
            .map_or(self.name, |(_, table)| table)
    }
}

pub(crate) const COLLECTIONS: &[Collection] = &[
    Collection {
        name: Exercise::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(Exercise::CONFLICT_RULE),
        indexed_fields: &["name", "archived"],
        checked_value: |value| checked(value, Exercise::validate),
        fixed_row_id: None,
        merge: None,
    },
    Collection {
        name: WorkoutTemplate::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(WorkoutTemplate::CONFLICT_RULE),
        indexed_fields: &["name"],
        checked_value: |value| checked(value, WorkoutTemplate::validate),
        fixed_row_id: None,
        merge: None,
    },
    Collection {
        name: Session::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(Session::CONFLICT_RULE),
        indexed_fields: &["started_at", "template_id"],
        checked_value: |value| checked(value, Session::validate),
        fixed_row_id: None,
        merge: Some(|winner, loser| {
            let winner: Session = decoded(winner)?;
            let merged = winner.clone().keeping_end_of(&decoded(loser)?);
            if merged == winner {
                return Ok(None);
            }
            serde_json::to_value(merged)
                .map(Some)
                .map_err(|error| error.to_string())
        }),
    },
    Collection {
        name: Set::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(Set::CONFLICT_RULE),
        indexed_fields: &["session_id", "exercise_id", "completed_at"],
        checked_value: |value| checked(value, Set::validate),
        fixed_row_id: Some(|value| {
            decoded::<Set>(value).map(|set| Set::row_id(set.session_id, set.order))
        }),
        merge: None,
    },
    Collection {
        name: BodyWeight::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(BodyWeight::CONFLICT_RULE),
        indexed_fields: &["time"],
        checked_value: |value| checked(value, BodyWeight::validate),
        fixed_row_id: None,
        merge: None,
    },
    Collection {
        name: Recovery::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(Recovery::CONFLICT_RULE),
        indexed_fields: &["muscle"],
        checked_value: |value| checked(value, Recovery::validate),
        fixed_row_id: Some(|value| {
            decoded::<Recovery>(value).map(|recovery| Recovery::row_id(recovery.muscle))
        }),
        merge: None,
    },
    Collection {
        name: Recipe::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Documents,
        indexed_fields: &[],
        checked_value: checked_kitchen::<Recipe>,
        fixed_row_id: None,
        merge: None,
    },
    Collection {
        name: PantryItem::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(ConflictRule::LastWriteWins),
        indexed_fields: &["name"],
        checked_value: checked_kitchen::<PantryItem>,
        fixed_row_id: None,
        merge: None,
    },
    Collection {
        name: Equipment::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(ConflictRule::LastWriteWins),
        indexed_fields: &["name"],
        checked_value: checked_kitchen::<Equipment>,
        fixed_row_id: None,
        merge: None,
    },
    Collection {
        name: CookingSession::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(ConflictRule::LastWriteWins),
        indexed_fields: &["recipe"],
        checked_value: checked_kitchen::<CookingSession>,
        fixed_row_id: None,
        merge: None,
    },
    Collection {
        name: Plan::COLLECTION_NAME,
        schema_version: 1,
        storage: Storage::Rows(ConflictRule::LastWriteWins),
        indexed_fields: &["recipe"],
        checked_value: checked_kitchen::<Plan>,
        fixed_row_id: None,
        merge: None,
    },
];

pub(crate) fn collection(name: &str) -> Option<&'static Collection> {
    COLLECTIONS
        .iter()
        .find(|collection| collection.name == name)
}

fn decoded<T: DeserializeOwned>(value: &Value) -> Result<T, String> {
    T::deserialize(value).map_err(|error| error.to_string())
}

fn checked<T: DeserializeOwned>(
    value: Value,
    validate: fn(&T) -> Result<(), ValidationError>,
) -> Result<Value, String> {
    validate(&decoded(&value)?).map_err(|error| error.to_string())?;
    Ok(value)
}

fn checked_kitchen<T: KitchenValue>(value: Value) -> Result<Value, String> {
    let typed = clipper_kitchen::decode::<T>(&value).map_err(|error| error.to_string())?;
    serde_json::to_value(typed).map_err(|error| error.to_string())
}
