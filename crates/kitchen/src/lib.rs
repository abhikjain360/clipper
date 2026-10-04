use std::{collections::BTreeSet, fmt};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            formatter.write_str(&self.message)
        } else {
            write!(formatter, "{}: {}", self.path, self.message)
        }
    }
}

pub trait KitchenValue: Serialize + DeserializeOwned {
    const COLLECTION_NAME: &'static str;

    fn validate(&self) -> Result<(), ValidationError>;
}

pub fn decode<T: KitchenValue>(value: &serde_json::Value) -> Result<T, ValidationError> {
    let decoded: T = serde_path_to_error::deserialize(value).map_err(|error| {
        let path = error.path().to_string();
        ValidationError {
            path: if path == "." { String::new() } else { path },
            message: error.inner().to_string(),
        }
    })?;
    decoded.validate()?;
    Ok(decoded)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub title: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuisine: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub servings: u32,
    pub active_minutes: f64,
    pub total_minutes: f64,
    #[serde(default)]
    pub equipment: Vec<String>,
    #[serde(default)]
    pub ingredients: Vec<Ingredient>,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nutrition_per_serving: Option<Nutrition>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(with = "date")]
    pub created_on: NaiveDate,
}

impl KitchenValue for Recipe {
    const COLLECTION_NAME: &'static str = "kitchen.recipes";

    fn validate(&self) -> Result<(), ValidationError> {
        validate_text(&self.title, "title")?;
        validate_text(&self.summary, "summary")?;
        validate_optional_text(self.cuisine.as_deref(), "cuisine")?;
        validate_texts(&self.tags, "tags")?;
        validate_positive_integer(u64::from(self.servings), "servings")?;
        validate_positive_number(self.active_minutes, "active_minutes")?;
        validate_positive_number(self.total_minutes, "total_minutes")?;
        validate_texts(&self.equipment, "equipment")?;
        if self.ingredients.is_empty() {
            return Err(invalid("ingredients", "needs at least one ingredient"));
        }
        for (index, ingredient) in self.ingredients.iter().enumerate() {
            ingredient.validate(&format!("ingredients[{index}]"))?;
        }
        if self.steps.is_empty() {
            return Err(invalid("steps", "needs at least one step"));
        }
        for (index, step) in self.steps.iter().enumerate() {
            step.validate(&format!("steps[{index}]"))?;
        }
        if let Some(nutrition) = &self.nutrition_per_serving {
            nutrition.validate("nutrition_per_serving")?;
        }
        validate_texts(&self.notes, "notes")?;

        let mut ids = BTreeSet::new();
        for (index, ingredient) in self.ingredients.iter().enumerate() {
            if !ids.insert(ingredient.id.as_str()) {
                return Err(invalid(
                    &format!("ingredients[{index}].id"),
                    &format!("duplicate ingredient id \"{}\"", ingredient.id),
                ));
            }
        }
        for (index, ingredient) in self.ingredients.iter().enumerate() {
            if ingredient.unit.is_some() && ingredient.amount.is_none() {
                return Err(invalid(
                    &format!("ingredients[{index}].unit"),
                    "a unit needs an amount",
                ));
            }
        }
        for (index, step) in self.steps.iter().enumerate() {
            let bytes = step.text.as_bytes();
            for (start, byte) in bytes.iter().enumerate() {
                if *byte != b'{' {
                    continue;
                }
                let mut end = start + 1;
                while end < bytes.len() && is_reference_byte(bytes[end]) {
                    end += 1;
                }
                if end > start + 1 && bytes.get(end) == Some(&b'}') {
                    let id = &step.text[start + 1..end];
                    if !ids.contains(id) {
                        return Err(invalid(
                            &format!("steps[{index}].text"),
                            &format!("{{{id}}} does not match any ingredient id"),
                        ));
                    }
                }
            }
        }
        if self.active_minutes > self.total_minutes {
            return Err(invalid(
                "active_minutes",
                "active time cannot exceed total time",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ingredient {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default = "default_scales")]
    pub scales: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub need_to_buy: bool,
}

impl Ingredient {
    fn validate(&self, path: &str) -> Result<(), ValidationError> {
        validate_ingredient_id(&self.id, &format!("{path}.id"))?;
        validate_text(&self.name, &format!("{path}.name"))?;
        if let Some(amount) = self.amount {
            validate_positive_number(amount, &format!("{path}.amount"))?;
        }
        validate_optional_text(self.unit.as_deref(), &format!("{path}.unit"))?;
        validate_optional_text(self.note.as_deref(), &format!("{path}.note"))?;
        validate_optional_text(self.group.as_deref(), &format!("{path}.group"))
    }
}

fn default_scales() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub text: String,
    #[serde(default)]
    pub timers: Vec<StepTimer>,
}

impl Step {
    fn validate(&self, path: &str) -> Result<(), ValidationError> {
        validate_text(&self.text, &format!("{path}.text"))?;
        if self.timers.is_empty() {
            return Err(invalid(
                &format!("{path}.timers"),
                "needs at least one timer",
            ));
        }
        for (index, timer) in self.timers.iter().enumerate() {
            let path = format!("{path}.timers[{index}]");
            validate_text(&timer.label, &format!("{path}.label"))?;
            validate_positive_number(timer.minutes, &format!("{path}.minutes"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepTimer {
    pub label: String,
    pub minutes: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nutrition {
    pub calories: f64,
    pub protein_grams: f64,
    pub carbs_grams: f64,
    pub fat_grams: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fiber_grams: Option<f64>,
}

impl Nutrition {
    fn validate(&self, path: &str) -> Result<(), ValidationError> {
        validate_nonnegative_number(self.calories, &format!("{path}.calories"))?;
        validate_nonnegative_number(self.protein_grams, &format!("{path}.protein_grams"))?;
        validate_nonnegative_number(self.carbs_grams, &format!("{path}.carbs_grams"))?;
        validate_nonnegative_number(self.fat_grams, &format!("{path}.fat_grams"))?;
        if let Some(fiber) = self.fiber_grams {
            validate_nonnegative_number(fiber, &format!("{path}.fiber_grams"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PantryItem {
    pub name: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_date"
    )]
    pub use_by: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl KitchenValue for PantryItem {
    const COLLECTION_NAME: &'static str = "kitchen.pantry";

    fn validate(&self) -> Result<(), ValidationError> {
        validate_text(&self.name, "name")?;
        validate_text(&self.category, "category")?;
        validate_optional_text(self.amount.as_deref(), "amount")?;
        validate_optional_text(self.notes.as_deref(), "notes")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Equipment {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl KitchenValue for Equipment {
    const COLLECTION_NAME: &'static str = "kitchen.equipment";

    fn validate(&self) -> Result<(), ValidationError> {
        validate_text(&self.name, "name")?;
        validate_optional_text(self.notes.as_deref(), "notes")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CookingSession {
    pub recipe: Uuid,
    pub recipe_revision: u64,
    pub servings: u32,
    #[serde(with = "time")]
    pub started_at: DateTime<Utc>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_time"
    )]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub steps_done: Vec<StepDone>,
    #[serde(default)]
    pub gathered: Vec<String>,
    #[serde(default)]
    pub timers: Vec<SessionTimer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl KitchenValue for CookingSession {
    const COLLECTION_NAME: &'static str = "kitchen.sessions";

    fn validate(&self) -> Result<(), ValidationError> {
        validate_recipe_id(self.recipe, "recipe")?;
        validate_positive_integer(self.recipe_revision, "recipe_revision")?;
        validate_positive_integer(u64::from(self.servings), "servings")?;
        if self.finished_at.is_some_and(|time| time < self.started_at) {
            return Err(invalid("finished_at", "must not be before started_at"));
        }
        let mut steps = BTreeSet::new();
        for (index, done) in self.steps_done.iter().enumerate() {
            if !steps.insert(done.step) {
                return Err(invalid(
                    &format!("steps_done[{index}].step"),
                    "step appears more than once",
                ));
            }
        }
        let mut gathered = BTreeSet::new();
        for (index, id) in self.gathered.iter().enumerate() {
            let path = format!("gathered[{index}]");
            validate_ingredient_id(id, &path)?;
            if !gathered.insert(id) {
                return Err(invalid(&path, &format!("duplicate ingredient id \"{id}\"")));
            }
        }
        let mut timers = BTreeSet::new();
        for (index, timer) in self.timers.iter().enumerate() {
            let path = format!("timers[{index}]");
            if timer.ends_at.is_some() == timer.remaining_ms.is_some() {
                return Err(invalid(&path, "needs either ends_at or remaining_ms"));
            }
            if !timers.insert((timer.step, timer.timer)) {
                return Err(invalid(&path, "step and timer pair appears more than once"));
            }
        }
        validate_optional_text(self.notes.as_deref(), "notes")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepDone {
    pub step: u32,
    #[serde(with = "time")]
    pub done_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTimer {
    pub step: u32,
    pub timer: u32,
    pub device_id: Uuid,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_time"
    )]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub recipe: Uuid,
    pub block: PlanBlock,
}

impl KitchenValue for Plan {
    const COLLECTION_NAME: &'static str = "kitchen.plans";

    fn validate(&self) -> Result<(), ValidationError> {
        validate_recipe_id(self.recipe, "recipe")?;
        validate_text(&self.block.item_id, "block.item_id")?;
        validate_text(&self.block.occurrence_key, "block.occurrence_key")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanBlock {
    pub item_id: String,
    pub occurrence_key: String,
}

fn invalid(path: &str, message: &str) -> ValidationError {
    ValidationError {
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

fn validate_text(text: &str, path: &str) -> Result<(), ValidationError> {
    if text.is_empty() {
        return Err(invalid(path, "must not be empty"));
    }
    Ok(())
}

fn validate_optional_text(text: Option<&str>, path: &str) -> Result<(), ValidationError> {
    if let Some(text) = text {
        validate_text(text, path)?;
    }
    Ok(())
}

fn validate_texts(texts: &[String], path: &str) -> Result<(), ValidationError> {
    for (index, text) in texts.iter().enumerate() {
        validate_text(text, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

fn validate_positive_integer(value: u64, path: &str) -> Result<(), ValidationError> {
    if value == 0 {
        return Err(invalid(path, "must be at least 1"));
    }
    Ok(())
}

fn validate_positive_number(value: f64, path: &str) -> Result<(), ValidationError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(invalid(path, "must be finite and greater than 0"));
    }
    Ok(())
}

fn validate_nonnegative_number(value: f64, path: &str) -> Result<(), ValidationError> {
    if !value.is_finite() || value < 0.0 {
        return Err(invalid(path, "must be finite and not negative"));
    }
    Ok(())
}

fn validate_ingredient_id(id: &str, path: &str) -> Result<(), ValidationError> {
    if !id.split('-').all(|word| {
        !word.is_empty()
            && word
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    }) {
        return Err(invalid(path, "must be lowercase words joined by hyphens"));
    }
    Ok(())
}

fn is_reference_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
}

fn validate_recipe_id(id: Uuid, path: &str) -> Result<(), ValidationError> {
    if id.get_version() != Some(uuid::Version::SortRand)
        || id.get_variant() != uuid::Variant::RFC4122
    {
        return Err(invalid(path, "must be a UUIDv7 document id"));
    }
    Ok(())
}

const DATE_FORMAT: &str = "must be a date written YYYY-MM-DD";
const TIME_FORMAT: &str = "must be an RFC 3339 time such as 2026-10-07T18:30:00Z";

fn parse_date(text: &str) -> Result<NaiveDate, &'static str> {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        });
    if !shaped {
        return Err(DATE_FORMAT);
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| DATE_FORMAT)
}

fn parse_time(text: &str) -> Result<DateTime<Utc>, &'static str> {
    if text.get(..10).is_none_or(|date| parse_date(date).is_err())
        || text.as_bytes().get(10) != Some(&b'T')
        || text.contains(char::is_whitespace)
    {
        return Err(TIME_FORMAT);
    }
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| TIME_FORMAT)
}

mod date {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(date: &NaiveDate, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&date.format("%Y-%m-%d"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<NaiveDate, D::Error> {
        super::parse_date(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

mod optional_date {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(
        date: &Option<NaiveDate>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match date {
            Some(date) => super::date::serialize(date, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<NaiveDate>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| super::parse_date(&text).map_err(D::Error::custom))
            .transpose()
    }
}

mod time {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(
        time: &DateTime<Utc>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&time.to_rfc3339_opts(SecondsFormat::Millis, true))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<DateTime<Utc>, D::Error> {
        super::parse_time(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

mod optional_time {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(
        time: &Option<DateTime<Utc>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match time {
            Some(time) => super::time::serialize(time, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<DateTime<Utc>>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| super::parse_time(&text).map_err(D::Error::custom))
            .transpose()
    }
}
