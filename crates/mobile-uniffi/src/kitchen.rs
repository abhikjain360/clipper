use clipper_app_types::{
    AppDocumentRevision, KitchenPantry, KitchenPantryChange, KitchenPlan, KitchenRecipe,
    KitchenRecipeList, KitchenSessionChange,
};

use crate::{MobileClipperClient, MobileError};

#[uniffi::export(async_runtime = "tokio")]
impl MobileClipperClient {
    pub async fn kitchen_recipes(
        &self,
        search: String,
        zone: String,
    ) -> Result<KitchenRecipeList, MobileError> {
        Ok(self.engine.kitchen_recipes(&search, &zone).await?)
    }

    pub async fn kitchen_recipe(
        &self,
        id: String,
        servings: Option<u32>,
        zone: String,
    ) -> Result<KitchenRecipe, MobileError> {
        Ok(self.engine.kitchen_recipe(&id, servings, &zone).await?)
    }

    pub async fn kitchen_recipe_history(
        &self,
        id: String,
    ) -> Result<Vec<AppDocumentRevision>, MobileError> {
        Ok(self
            .engine
            .app_document_history("kitchen.recipes", &id)
            .await?)
    }

    pub async fn kitchen_recipe_revision(
        &self,
        id: String,
        revision: u64,
        servings: Option<u32>,
    ) -> Result<KitchenRecipe, MobileError> {
        Ok(self
            .engine
            .kitchen_recipe_revision(&id, revision, servings)
            .await?)
    }

    pub async fn kitchen_change_session(
        &self,
        recipe_id: String,
        revision: u64,
        servings: u32,
        change: KitchenSessionChange,
    ) -> Result<(), MobileError> {
        Ok(self
            .engine
            .kitchen_change_session(&recipe_id, revision, servings, change)
            .await?)
    }

    pub async fn kitchen_pantry(&self) -> Result<KitchenPantry, MobileError> {
        Ok(self.engine.kitchen_pantry().await?)
    }

    pub async fn kitchen_change_pantry(
        &self,
        change: KitchenPantryChange,
    ) -> Result<(), MobileError> {
        Ok(self.engine.kitchen_change_pantry(change).await?)
    }

    pub async fn kitchen_plans(&self) -> Result<Vec<KitchenPlan>, MobileError> {
        Ok(self.engine.kitchen_plans().await?)
    }
}
