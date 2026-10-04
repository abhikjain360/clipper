use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use clipper_app_types::{
    KitchenBlock, KitchenEquipment, KitchenIngredient, KitchenIngredientGroup, KitchenNutrition,
    KitchenPantry, KitchenPantryCategory, KitchenPantryChange, KitchenPantryItem,
    KitchenPastSession, KitchenPlan, KitchenRecipe, KitchenRecipeList, KitchenRecipeSummary,
    KitchenSession, KitchenSessionChange, KitchenStep, KitchenTimer, KitchenTimerAction,
    KitchenTimerState,
};
use clipper_kitchen::{
    CookingSession, Equipment, Ingredient, KitchenValue, PantryItem, Plan, Recipe, TimerState,
    scale_factor,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use uuid::Uuid;

use super::*;

static KITCHEN_WRITES: Mutex<()> = Mutex::const_new(());
const COMING_BLOCK_DAYS: i64 = 60;
const TIMER_RINGS_FOR_MINUTES: i64 = 10;

struct Stored<T> {
    id: Uuid,
    revision: u64,
    written_at: String,
    value: T,
}

impl SyncEngine {
    pub async fn kitchen_recipes(
        &self,
        search: &str,
        zone: &str,
    ) -> Result<KitchenRecipeList, ClientError> {
        self.run_work(None, async {
            let recipes = self.kitchen_rows::<Recipe>("").await?;
            let cooking: HashSet<Uuid> = self
                .open_sessions("")
                .await?
                .into_iter()
                .map(|session| session.value.recipe)
                .collect();
            let blocks = self.coming_blocks(zone).await;
            let summary = |recipe: &Stored<Recipe>| KitchenRecipeSummary {
                id: recipe.id.to_string(),
                revision: recipe.revision,
                title: recipe.value.title.clone(),
                summary: recipe.value.summary.clone(),
                cuisine: recipe.value.cuisine.clone(),
                tags: recipe.value.tags.clone(),
                total_minutes: recipe.value.total_minutes,
                created_on: recipe.value.created_on.to_string(),
                cooking: cooking.contains(&recipe.id),
                next_block: blocks
                    .iter()
                    .find(|(_, planned)| *planned == recipe.id)
                    .map(|(block, _)| block.clone()),
            };
            let mut ordered: Vec<&Stored<Recipe>> = recipes.iter().collect();
            ordered.sort_by(|a, b| {
                (b.value.created_on, &b.written_at).cmp(&(a.value.created_on, &a.written_at))
            });
            let words: Vec<String> = search.split_whitespace().map(str::to_lowercase).collect();
            let next_block = blocks.first().map(|(block, _)| block.clone());
            let next_block_recipes = next_block
                .as_ref()
                .map(|next| {
                    ordered
                        .iter()
                        .filter(|recipe| {
                            blocks.iter().any(|(block, planned)| {
                                block.item_id == next.item_id
                                    && block.occurrence_key == next.occurrence_key
                                    && *planned == recipe.id
                            })
                        })
                        .map(|recipe| summary(recipe))
                        .collect()
                })
                .unwrap_or_default();
            Ok(KitchenRecipeList {
                next_block,
                next_block_recipes,
                recipes: ordered
                    .into_iter()
                    .filter(|recipe| matches_search(&recipe.value, &words))
                    .map(summary)
                    .collect(),
            })
        })
        .await
    }

    pub async fn kitchen_recipe(
        &self,
        id: &str,
        servings: Option<u32>,
        zone: &str,
    ) -> Result<KitchenRecipe, ClientError> {
        self.run_work(None, async {
            let id = parse_kitchen_id(id)?;
            let current = self.current_recipe(id).await?;
            let mut sessions = self.sessions_of(id).await?;
            sessions
                .sort_by_key(|session| std::cmp::Reverse((session.value.started_at, session.id)));
            let open = sessions.iter().find(|session| session.value.is_open());
            let (revision, recipe) = match (open, &current) {
                (Some(open), _) => {
                    let revision = open.value.recipe_revision;
                    (
                        revision,
                        self.session_recipe(id, revision, current.as_ref()).await?,
                    )
                }
                (None, Some(current)) => (current.revision, current.value.clone()),
                (None, None) => return Err(ClientError::ItemNotFound { id: id.to_string() }),
            };
            let device = self.this_device().await?;
            let mut view = recipe_view(id, revision, &recipe, servings, open, device, Utc::now());
            view.newer_revision = current
                .as_ref()
                .filter(|current| current.revision > revision)
                .map(|current| current.revision);
            view.deleted = current.is_none();
            view.past_sessions = sessions
                .iter()
                .filter_map(|session| {
                    let finished_at = session.value.finished_at?;
                    Some(KitchenPastSession {
                        id: session.id.to_string(),
                        servings: session.value.servings,
                        recipe_revision: session.value.recipe_revision,
                        started_at_millis: session.value.started_at.timestamp_millis(),
                        finished_at_millis: finished_at.timestamp_millis(),
                        duration_minutes: u32::try_from(
                            (finished_at - session.value.started_at).num_minutes(),
                        )
                        .unwrap_or_default(),
                        notes: session.value.notes.clone(),
                    })
                })
                .collect();
            view.coming_blocks = self
                .coming_blocks(zone)
                .await
                .into_iter()
                .filter(|(_, planned)| *planned == id)
                .map(|(block, _)| block)
                .collect();
            Ok(view)
        })
        .await
    }

    pub async fn kitchen_recipe_revision(
        &self,
        id: &str,
        revision: u64,
        servings: Option<u32>,
    ) -> Result<KitchenRecipe, ClientError> {
        self.run_work(None, async {
            let id = parse_kitchen_id(id)?;
            let recipe = self.recipe_revision(id, revision).await?;
            let mut view = recipe_view(
                id,
                revision,
                &recipe,
                servings,
                None,
                Uuid::nil(),
                Utc::now(),
            );
            view.read_only = true;
            Ok(view)
        })
        .await
    }

    pub async fn kitchen_change_session(
        &self,
        recipe_id: &str,
        revision: u64,
        servings: u32,
        change: KitchenSessionChange,
    ) -> Result<(), ClientError> {
        self.run_work(None, async {
            let _writing = KITCHEN_WRITES.lock().await;
            let recipe_id = parse_kitchen_id(recipe_id)?;
            let mut open: Vec<Stored<CookingSession>> = self
                .sessions_of(recipe_id)
                .await?
                .into_iter()
                .filter(|session| session.value.is_open())
                .collect();
            open.sort_by_key(|session| std::cmp::Reverse((session.value.started_at, session.id)));
            let mut open = open.into_iter();
            let newest = open.next();
            let older: Vec<Uuid> = open.map(|session| session.id).collect();
            let now = Utc::now();
            let (id, mut session) = match (newest, &change) {
                (Some(newest), KitchenSessionChange::Discard) => {
                    self.delete_sessions(older.iter().copied().chain([newest.id]))
                        .await?;
                    return Ok(());
                }
                (Some(_), KitchenSessionChange::Start) => {
                    self.delete_sessions(older).await?;
                    return Ok(());
                }
                (Some(newest), _) => (Some(newest.id), newest.value),
                (
                    None,
                    KitchenSessionChange::Start
                    | KitchenSessionChange::Gathered { .. }
                    | KitchenSessionChange::StepDone { .. }
                    | KitchenSessionChange::Timer { .. },
                ) => {
                    let current = self.current_recipe(recipe_id).await?.ok_or_else(|| {
                        ClientError::ItemNotFound {
                            id: recipe_id.to_string(),
                        }
                    })?;
                    if revision == 0 || revision > current.revision {
                        return Err(ClientError::InvalidArgument(format!(
                            "recipe {recipe_id} has no revision {revision}"
                        )));
                    }
                    (
                        None,
                        CookingSession::new(recipe_id, revision, servings, now),
                    )
                }
                (None, _) => return Err(no_open_session()),
            };
            match change {
                KitchenSessionChange::Start | KitchenSessionChange::Discard => {}
                KitchenSessionChange::Servings { servings } => session.servings = servings,
                KitchenSessionChange::Gathered {
                    ingredient,
                    gathered,
                } => session.set_gathered(&ingredient, gathered),
                KitchenSessionChange::StepDone { step, done } => {
                    session.set_step_done(step, done, now);
                }
                KitchenSessionChange::Timer {
                    step,
                    timer,
                    action,
                } => match action {
                    KitchenTimerAction::Start => {
                        let current = self.current_recipe(recipe_id).await?;
                        let minutes = self
                            .session_recipe(recipe_id, session.recipe_revision, current.as_ref())
                            .await?
                            .steps
                            .get(step as usize)
                            .and_then(|entry| entry.timers.get(timer as usize))
                            .map(|entry| entry.minutes)
                            .ok_or_else(|| {
                                ClientError::InvalidArgument(format!(
                                    "step {step} has no timer {timer}"
                                ))
                            })?;
                        let device = self.this_device().await?;
                        session.start_timer(step, timer, minutes, device, now);
                    }
                    KitchenTimerAction::Pause => session.pause_timer(step, timer, now),
                    KitchenTimerAction::Resume => session.resume_timer(step, timer, now),
                    KitchenTimerAction::AddMinute => session.add_minute(step, timer, now),
                    KitchenTimerAction::Clear => session.clear_timer(step, timer),
                },
                KitchenSessionChange::Finish { notes } => session.finish(notes, now),
            }
            let value = serde_json::to_value(&session)
                .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
            self.write_app_data(
                CookingSession::COLLECTION_NAME,
                id.map(|id| id.to_string()).as_deref(),
                AppDataWrite::Value(value),
            )
            .await?;
            self.delete_sessions(older).await
        })
        .await
    }

    pub async fn kitchen_pantry(&self) -> Result<KitchenPantry, ClientError> {
        self.run_work(None, async {
            let items = self
                .kitchen_rows_of::<PantryItem>(PantryItem::COLLECTION_NAME, "")
                .await?;
            let equipment = self
                .kitchen_rows_of::<Equipment>(Equipment::COLLECTION_NAME, "")
                .await?;
            let item_names = name_counts(items.iter().map(|item| item.value.name.as_str()));
            let equipment_names =
                name_counts(equipment.iter().map(|entry| entry.value.name.as_str()));
            let mut categories: BTreeMap<String, KitchenPantryCategory> = BTreeMap::new();
            for item in &items {
                categories
                    .entry(item.value.category.trim().to_lowercase())
                    .or_insert_with(|| KitchenPantryCategory {
                        name: item.value.category.trim().to_string(),
                        items: Vec::new(),
                    })
                    .items
                    .push(KitchenPantryItem {
                        id: item.id.to_string(),
                        name: item.value.name.clone(),
                        category: item.value.category.clone(),
                        amount: item.value.amount.clone(),
                        use_by: item.value.use_by.map(|date| date.to_string()),
                        notes: item.value.notes.clone(),
                        listed_twice: item_names
                            .get(&name_key(&item.value.name))
                            .is_some_and(|count| *count > 1),
                    });
            }
            let mut categories: Vec<KitchenPantryCategory> = categories.into_values().collect();
            for category in &mut categories {
                category.items.sort_by(|a, b| {
                    (a.use_by.is_none(), &a.use_by, a.name.to_lowercase()).cmp(&(
                        b.use_by.is_none(),
                        &b.use_by,
                        b.name.to_lowercase(),
                    ))
                });
            }
            let mut equipment: Vec<KitchenEquipment> = equipment
                .iter()
                .map(|entry| KitchenEquipment {
                    id: entry.id.to_string(),
                    name: entry.value.name.clone(),
                    notes: entry.value.notes.clone(),
                    listed_twice: equipment_names
                        .get(&name_key(&entry.value.name))
                        .is_some_and(|count| *count > 1),
                })
                .collect();
            equipment.sort_by_key(|entry| entry.name.to_lowercase());
            Ok(KitchenPantry {
                categories,
                equipment,
            })
        })
        .await
    }

    pub async fn kitchen_change_pantry(
        &self,
        change: KitchenPantryChange,
    ) -> Result<(), ClientError> {
        match &change {
            KitchenPantryChange::SaveItem { id, name, .. } => {
                self.refuse_listed_name::<PantryItem>(id.as_deref(), name, |item| &item.name)
                    .await?;
            }
            KitchenPantryChange::SaveEquipment { id, name, .. } => {
                self.refuse_listed_name::<Equipment>(id.as_deref(), name, |entry| &entry.name)
                    .await?;
            }
            KitchenPantryChange::DeleteItem { .. }
            | KitchenPantryChange::DeleteEquipment { .. } => {}
        }
        let (collection, id, write) = match change {
            KitchenPantryChange::SaveItem {
                id,
                name,
                category,
                amount,
                use_by,
                notes,
            } => (
                PantryItem::COLLECTION_NAME,
                id,
                AppDataWrite::Value(json!({
                    "name": name.trim(),
                    "category": category.trim(),
                    "amount": filled(amount),
                    "use_by": filled(use_by),
                    "notes": filled(notes),
                })),
            ),
            KitchenPantryChange::DeleteItem { id } => {
                (PantryItem::COLLECTION_NAME, Some(id), AppDataWrite::Delete)
            }
            KitchenPantryChange::SaveEquipment { id, name, notes } => (
                Equipment::COLLECTION_NAME,
                id,
                AppDataWrite::Value(json!({
                    "name": name.trim(),
                    "notes": filled(notes),
                })),
            ),
            KitchenPantryChange::DeleteEquipment { id } => {
                (Equipment::COLLECTION_NAME, Some(id), AppDataWrite::Delete)
            }
        };
        self.write_app_data(collection, id.as_deref(), write)
            .await
            .map(drop)
    }

    pub async fn kitchen_plans(&self) -> Result<Vec<KitchenPlan>, ClientError> {
        self.run_work(None, async {
            let titles: BTreeMap<Uuid, String> = self
                .kitchen_rows::<Recipe>("")
                .await?
                .into_iter()
                .map(|recipe| (recipe.id, recipe.value.title))
                .collect();
            Ok(self
                .kitchen_rows_of::<Plan>(Plan::COLLECTION_NAME, "")
                .await?
                .into_iter()
                .filter_map(|plan| {
                    Some(KitchenPlan {
                        item_id: plan.value.block.item_id,
                        occurrence_key: plan.value.block.occurrence_key,
                        recipe_id: plan.value.recipe.to_string(),
                        title: titles.get(&plan.value.recipe)?.clone(),
                    })
                })
                .collect())
        })
        .await
    }

    pub(super) async fn kitchen_timer_alarms(
        &self,
        device: DeviceId,
        now: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Vec<AlarmView> {
        let Ok(sessions) = self.open_sessions("").await else {
            return Vec::new();
        };
        let Ok(recipes) = self.kitchen_rows::<Recipe>("").await else {
            return Vec::new();
        };
        let recipes: BTreeMap<Uuid, Stored<Recipe>> = recipes
            .into_iter()
            .map(|recipe| (recipe.id, recipe))
            .collect();
        let mut newest: BTreeMap<Uuid, Stored<CookingSession>> = BTreeMap::new();
        for session in sessions {
            match newest.get(&session.value.recipe) {
                Some(kept)
                    if (kept.value.started_at, kept.id)
                        >= (session.value.started_at, session.id) => {}
                _ => {
                    newest.insert(session.value.recipe, session);
                }
            }
        }
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let earlier = self.recipe_revisions.lock().await;
        let device = device.into_uuid();
        let ringing_from = now - TimeDelta::minutes(TIMER_RINGS_FOR_MINUTES);
        let mut alarms = Vec::new();
        for (recipe_id, session) in newest {
            let Some(current) = recipes.get(&recipe_id) else {
                continue;
            };
            let recipe = if current.revision == session.value.recipe_revision {
                Some(&current.value)
            } else {
                earlier.get(&(epoch, recipe_id, session.value.recipe_revision))
            };
            for (step, timer, ends_at) in session.value.running_timers_on(device) {
                if ends_at <= ringing_from || ends_at > until {
                    continue;
                }
                let label = recipe
                    .and_then(|recipe| recipe.steps.get(step as usize))
                    .and_then(|entry| entry.timers.get(timer as usize))
                    .map_or("Timer", |entry| entry.label.as_str());
                alarms.push(AlarmView {
                    item_id: session.id.to_string(),
                    occurrence_key: format!("timer:{step}:{timer}"),
                    label: format!("{label} · {}", current.value.title),
                    fire_at_millis: ends_at.timestamp_millis(),
                    occurrence_start_millis: ends_at.timestamp_millis(),
                    can_snooze: false,
                });
            }
        }
        alarms
    }

    async fn refuse_listed_name<T: KitchenValue>(
        &self,
        id: Option<&str>,
        name: &str,
        name_of: fn(&T) -> &String,
    ) -> Result<(), ClientError> {
        let key = name_key(name);
        let listed = self.kitchen_rows::<T>("").await?.into_iter().any(|row| {
            Some(row.id.to_string().as_str()) != id && name_key(name_of(&row.value)) == key
        });
        if listed {
            return Err(ClientError::InvalidArgument(format!(
                "{} is already listed; edit that entry instead",
                name.trim()
            )));
        }
        Ok(())
    }

    async fn this_device(&self) -> Result<Uuid, ClientError> {
        self.current_device_id()
            .await?
            .parse()
            .map_err(|_| ClientError::NotAuthenticated)
    }

    async fn current_recipe(&self, id: Uuid) -> Result<Option<Stored<Recipe>>, ClientError> {
        Ok(self
            .kitchen_rows::<Recipe>(&format!("WHERE id = '{id}'"))
            .await?
            .pop())
    }

    async fn session_recipe(
        &self,
        id: Uuid,
        revision: u64,
        current: Option<&Stored<Recipe>>,
    ) -> Result<Recipe, ClientError> {
        let recipe = match current.filter(|current| current.revision == revision) {
            Some(current) => current.value.clone(),
            None => return self.recipe_revision(id, revision).await,
        };
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        self.recipe_revisions
            .lock()
            .await
            .insert((epoch, id, revision), recipe.clone());
        Ok(recipe)
    }

    async fn recipe_revision(&self, id: Uuid, revision: u64) -> Result<Recipe, ClientError> {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        if let Some(recipe) = self
            .recipe_revisions
            .lock()
            .await
            .get(&(epoch, id, revision))
        {
            return Ok(recipe.clone());
        }
        let value = self
            .app_document_revision(Recipe::COLLECTION_NAME, &id.to_string(), revision)
            .await?;
        let recipe = serde_json::from_value::<Recipe>(value).map_err(|error| {
            ClientError::UnexpectedResponse(format!(
                "revision {revision} of recipe {id} does not decode: {error}"
            ))
        })?;
        self.recipe_revisions
            .lock()
            .await
            .insert((epoch, id, revision), recipe.clone());
        Ok(recipe)
    }

    async fn sessions_of(&self, recipe: Uuid) -> Result<Vec<Stored<CookingSession>>, ClientError> {
        self.kitchen_rows::<CookingSession>(&format!(
            "WHERE json_extract(value, '$.recipe') = '{recipe}'"
        ))
        .await
    }

    async fn delete_sessions(
        &self,
        ids: impl IntoIterator<Item = Uuid>,
    ) -> Result<(), ClientError> {
        for id in ids {
            self.write_app_data(
                CookingSession::COLLECTION_NAME,
                Some(&id.to_string()),
                AppDataWrite::Delete,
            )
            .await?;
        }
        Ok(())
    }

    async fn open_sessions(
        &self,
        filter: &str,
    ) -> Result<Vec<Stored<CookingSession>>, ClientError> {
        self.kitchen_rows::<CookingSession>(&format!(
            "WHERE json_extract(value, '$.finished_at') IS NULL {filter}"
        ))
        .await
    }

    async fn kitchen_rows<T: KitchenValue>(
        &self,
        filter: &str,
    ) -> Result<Vec<Stored<T>>, ClientError> {
        self.kitchen_rows_of(T::COLLECTION_NAME, filter).await
    }

    async fn kitchen_rows_of<T: DeserializeOwned>(
        &self,
        collection: &str,
        filter: &str,
    ) -> Result<Vec<Stored<T>>, ClientError> {
        let rows = self
            .query_app_data(&format!(
                "SELECT id, revision, written_at, value FROM {collection} {filter}"
            ))
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let id = row.get("id")?.as_str()?.parse().ok()?;
                let value = match serde_json::from_str(row.get("value")?.as_str()?) {
                    Ok(value) => value,
                    Err(error) => {
                        warn!(%id, collection, "Skipping a kitchen record that does not decode: {error}");
                        return None;
                    }
                };
                Some(Stored {
                    id,
                    revision: row.get("revision")?.as_u64()?,
                    written_at: row.get("written_at")?.as_str()?.to_string(),
                    value,
                })
            })
            .collect())
    }

    async fn coming_blocks(&self, zone: &str) -> Vec<(KitchenBlock, Uuid)> {
        let plans = match self
            .kitchen_rows_of::<Plan>(Plan::COLLECTION_NAME, "")
            .await
        {
            Ok(plans) if !plans.is_empty() => plans,
            Ok(_) => return Vec::new(),
            Err(error) => {
                warn!("Failed to read kitchen plans: {error}");
                return Vec::new();
            }
        };
        let now = Utc::now();
        let occurrences = match self
            .expand_schedule(
                &now.to_rfc3339(),
                &(now + TimeDelta::days(COMING_BLOCK_DAYS)).to_rfc3339(),
                zone,
            )
            .await
        {
            Ok(occurrences) => occurrences,
            Err(error) => {
                warn!("Failed to expand the schedule for kitchen plans: {error}");
                return Vec::new();
            }
        };
        let mut blocks: Vec<(KitchenBlock, Uuid)> = plans
            .iter()
            .filter_map(|plan| {
                let occurrence = occurrences.iter().find(|occurrence| {
                    occurrence.item_id == plan.value.block.item_id
                        && occurrence.occurrence_key == plan.value.block.occurrence_key
                })?;
                Some((
                    KitchenBlock {
                        item_id: occurrence.item_id.clone(),
                        occurrence_key: occurrence.occurrence_key.clone(),
                        title: occurrence.title.clone(),
                        start_millis: millis_of(&occurrence.start)?,
                        end_millis: millis_of(&occurrence.end)?,
                    },
                    plan.value.recipe,
                ))
            })
            .collect();
        blocks.sort_by_key(|(block, _)| block.start_millis);
        blocks
    }
}

fn recipe_view(
    id: Uuid,
    revision: u64,
    value: &Recipe,
    servings: Option<u32>,
    session: Option<&Stored<CookingSession>>,
    device: Uuid,
    now: DateTime<Utc>,
) -> KitchenRecipe {
    let servings = session
        .map(|session| session.value.servings)
        .or(servings)
        .unwrap_or(value.servings)
        .max(1);
    let factor = scale_factor(value.servings, servings);
    let ingredient = |entry: &Ingredient| KitchenIngredient {
        id: entry.id.clone(),
        name: entry.name.clone(),
        quantity: entry.quantity(factor),
        note: entry.note.clone(),
        optional: entry.optional,
        need_to_buy: entry.need_to_buy,
        gathered: session.is_some_and(|session| session.value.is_gathered(&entry.id)),
    };
    let mut groups: Vec<KitchenIngredientGroup> = Vec::new();
    for entry in &value.ingredients {
        match groups.iter_mut().find(|group| group.name == entry.group) {
            Some(group) => group.ingredients.push(ingredient(entry)),
            None => groups.push(KitchenIngredientGroup {
                name: entry.group.clone(),
                ingredients: vec![ingredient(entry)],
            }),
        }
    }
    let steps = value
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            KitchenStep {
                index,
                text: value.step_text(index as usize, factor),
                done: session.is_some_and(|session| session.value.step_done_at(index).is_some()),
                timers: step
                    .timers
                    .iter()
                    .enumerate()
                    .map(|(timer_index, timer)| {
                        let timer_index = u32::try_from(timer_index).unwrap_or(u32::MAX);
                        let state = session.map_or(TimerState::Idle, |session| {
                            session.value.timer_state(index, timer_index, now)
                        });
                        let (state, ends_at_millis, remaining_millis) = match state {
                            TimerState::Idle => (KitchenTimerState::Idle, None, None),
                            TimerState::Running { ends_at } => (
                                KitchenTimerState::Running,
                                Some(ends_at.timestamp_millis()),
                                None,
                            ),
                            TimerState::Paused { remaining_ms } => (
                                KitchenTimerState::Paused,
                                None,
                                Some(i64::try_from(remaining_ms).unwrap_or(i64::MAX)),
                            ),
                            TimerState::Done { ended_at } => (
                                KitchenTimerState::Done,
                                Some(ended_at.timestamp_millis()),
                                None,
                            ),
                        };
                        KitchenTimer {
                            index: timer_index,
                            label: timer.label.clone(),
                            minutes: timer.minutes,
                            state,
                            ends_at_millis,
                            remaining_millis,
                            rings_here: session.is_some_and(|session| {
                                session.value.timer_device(index, timer_index) == Some(device)
                            }),
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    KitchenRecipe {
        id: id.to_string(),
        revision,
        newer_revision: None,
        deleted: false,
        read_only: false,
        title: value.title.clone(),
        summary: value.summary.clone(),
        cuisine: value.cuisine.clone(),
        tags: value.tags.clone(),
        recipe_servings: value.servings,
        servings,
        active_minutes: value.active_minutes,
        total_minutes: value.total_minutes,
        created_on: value.created_on.to_string(),
        nutrition: value
            .nutrition_per_serving
            .as_ref()
            .map(|nutrition| KitchenNutrition {
                calories: nutrition.calories,
                protein_grams: nutrition.protein_grams,
                carbs_grams: nutrition.carbs_grams,
                fat_grams: nutrition.fat_grams,
                fiber_grams: nutrition.fiber_grams,
            }),
        shopping: value
            .ingredients
            .iter()
            .filter(|entry| entry.need_to_buy)
            .map(ingredient)
            .collect(),
        groups,
        equipment: value.equipment.clone(),
        steps,
        notes: value.notes.clone(),
        session: session.map(|session| KitchenSession {
            id: session.id.to_string(),
            servings: session.value.servings,
            recipe_revision: session.value.recipe_revision,
            started_at_millis: session.value.started_at.timestamp_millis(),
        }),
        past_sessions: Vec::new(),
        coming_blocks: Vec::new(),
    }
}

fn matches_search(recipe: &Recipe, words: &[String]) -> bool {
    let fields: Vec<String> = [recipe.title.as_str(), recipe.summary.as_str()]
        .into_iter()
        .chain(recipe.cuisine.as_deref())
        .chain(recipe.tags.iter().map(String::as_str))
        .map(str::to_lowercase)
        .collect();
    words
        .iter()
        .all(|word| fields.iter().any(|field| field.contains(word.as_str())))
}

fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn name_counts<'a>(names: impl Iterator<Item = &'a str>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for name in names {
        *counts.entry(name_key(name)).or_insert(0) += 1;
    }
    counts
}

fn filled(text: Option<String>) -> Option<String> {
    text.map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn millis_of(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn parse_kitchen_id(id: &str) -> Result<Uuid, ClientError> {
    id.parse().map_err(|source| ClientError::InvalidId {
        kind: "kitchen id",
        source,
    })
}

fn no_open_session() -> ClientError {
    ClientError::InvalidArgument("no cooking session is open for this recipe".into())
}
