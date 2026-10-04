use clipper_gym::{
    BodyWeight, ConflictRule, Exercise, Recovery, Session, Set, ValidationError, WorkoutTemplate,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

type RowIdFromValue = fn(&Value) -> Result<Uuid, String>;

pub(crate) struct Collection {
    pub name: &'static str,
    pub schema_version: u64,
    pub conflict_rule: ConflictRule,
    pub indexed_fields: &'static [&'static str],
    pub validate: fn(&Value) -> Result<(), String>,
    pub fixed_row_id: Option<RowIdFromValue>,
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
        conflict_rule: Exercise::CONFLICT_RULE,
        indexed_fields: &["name", "archived"],
        validate: |value| checked(value, Exercise::validate),
        fixed_row_id: None,
    },
    Collection {
        name: WorkoutTemplate::COLLECTION_NAME,
        schema_version: 1,
        conflict_rule: WorkoutTemplate::CONFLICT_RULE,
        indexed_fields: &["name"],
        validate: |value| checked(value, WorkoutTemplate::validate),
        fixed_row_id: None,
    },
    Collection {
        name: Session::COLLECTION_NAME,
        schema_version: 1,
        conflict_rule: Session::CONFLICT_RULE,
        indexed_fields: &["started_at", "template_id"],
        validate: |value| checked(value, Session::validate),
        fixed_row_id: None,
    },
    Collection {
        name: Set::COLLECTION_NAME,
        schema_version: 1,
        conflict_rule: Set::CONFLICT_RULE,
        indexed_fields: &["session_id", "exercise_id", "completed_at"],
        validate: |value| checked(value, Set::validate),
        fixed_row_id: None,
    },
    Collection {
        name: BodyWeight::COLLECTION_NAME,
        schema_version: 1,
        conflict_rule: BodyWeight::CONFLICT_RULE,
        indexed_fields: &["time"],
        validate: |value| checked(value, BodyWeight::validate),
        fixed_row_id: None,
    },
    Collection {
        name: Recovery::COLLECTION_NAME,
        schema_version: 1,
        conflict_rule: Recovery::CONFLICT_RULE,
        indexed_fields: &["muscle"],
        validate: |value| checked(value, Recovery::validate),
        fixed_row_id: Some(|value| {
            decoded::<Recovery>(value).map(|recovery| Recovery::row_id(recovery.muscle))
        }),
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
    value: &Value,
    validate: fn(&T) -> Result<(), ValidationError>,
) -> Result<(), String> {
    validate(&decoded(value)?).map_err(|error| error.to_string())
}
