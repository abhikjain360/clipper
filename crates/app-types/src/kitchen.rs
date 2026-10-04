use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenBlock {
    pub item_id: String,
    pub occurrence_key: String,
    pub title: String,
    pub start_millis: i64,
    pub end_millis: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenRecipeSummary {
    pub id: String,
    pub revision: u64,
    pub title: String,
    pub summary: String,
    pub cuisine: Option<String>,
    pub tags: Vec<String>,
    pub total_minutes: f64,
    pub created_on: String,
    pub cooking: bool,
    pub next_block: Option<KitchenBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenRecipeList {
    pub next_block: Option<KitchenBlock>,
    pub next_block_recipes: Vec<KitchenRecipeSummary>,
    pub recipes: Vec<KitchenRecipeSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenNutrition {
    pub calories: f64,
    pub protein_grams: f64,
    pub carbs_grams: f64,
    pub fat_grams: f64,
    pub fiber_grams: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenIngredient {
    pub id: String,
    pub name: String,
    pub quantity: String,
    pub note: Option<String>,
    pub optional: bool,
    pub need_to_buy: bool,
    pub gathered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenIngredientGroup {
    pub name: Option<String>,
    pub ingredients: Vec<KitchenIngredient>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum KitchenTimerState {
    Idle,
    Running,
    Paused,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenTimer {
    pub index: u32,
    pub label: String,
    pub minutes: f64,
    pub state: KitchenTimerState,
    pub ends_at_millis: Option<i64>,
    pub remaining_millis: Option<i64>,
    pub rings_here: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenStep {
    pub index: u32,
    pub text: String,
    pub done: bool,
    pub timers: Vec<KitchenTimer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenSession {
    pub id: String,
    pub servings: u32,
    pub recipe_revision: u64,
    pub started_at_millis: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenPastSession {
    pub id: String,
    pub servings: u32,
    pub recipe_revision: u64,
    pub started_at_millis: i64,
    pub finished_at_millis: i64,
    pub duration_minutes: u32,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenRecipe {
    pub id: String,
    pub revision: u64,
    pub read_only: bool,
    pub title: String,
    pub summary: String,
    pub cuisine: Option<String>,
    pub tags: Vec<String>,
    pub recipe_servings: u32,
    pub servings: u32,
    pub active_minutes: f64,
    pub total_minutes: f64,
    pub created_on: String,
    pub nutrition: Option<KitchenNutrition>,
    pub groups: Vec<KitchenIngredientGroup>,
    pub shopping: Vec<KitchenIngredient>,
    pub equipment: Vec<String>,
    pub steps: Vec<KitchenStep>,
    pub notes: Vec<String>,
    pub session: Option<KitchenSession>,
    pub past_sessions: Vec<KitchenPastSession>,
    pub coming_blocks: Vec<KitchenBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenPantryItem {
    pub id: String,
    pub name: String,
    pub category: String,
    pub amount: Option<String>,
    pub use_by: Option<String>,
    pub notes: Option<String>,
    pub listed_twice: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenPantryCategory {
    pub name: String,
    pub items: Vec<KitchenPantryItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenEquipment {
    pub id: String,
    pub name: String,
    pub notes: Option<String>,
    pub listed_twice: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenPantry {
    pub categories: Vec<KitchenPantryCategory>,
    pub equipment: Vec<KitchenEquipment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KitchenPlan {
    pub item_id: String,
    pub occurrence_key: String,
    pub recipe_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum KitchenTimerAction {
    Start,
    Pause,
    Resume,
    AddMinute,
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum KitchenSessionChange {
    Start {
        servings: u32,
    },
    Servings {
        servings: u32,
    },
    Gathered {
        ingredient: String,
        gathered: bool,
    },
    StepDone {
        step: u32,
        done: bool,
    },
    Timer {
        step: u32,
        timer: u32,
        action: KitchenTimerAction,
    },
    Finish {
        notes: Option<String>,
    },
    Discard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum KitchenPantryChange {
    SaveItem {
        id: Option<String>,
        name: String,
        category: String,
        amount: Option<String>,
        use_by: Option<String>,
        notes: Option<String>,
    },
    DeleteItem {
        id: String,
    },
    SaveEquipment {
        id: Option<String>,
        name: String,
        notes: Option<String>,
    },
    DeleteEquipment {
        id: String,
    },
}
