import type { AppDocumentRevision } from "./types";

export type KitchenBlock = {
  item_id: string;
  occurrence_key: string;
  title: string;
  start_millis: number;
  end_millis: number;
};

export type KitchenRecipeSummary = {
  id: string;
  revision: number;
  title: string;
  summary: string;
  cuisine: string | null;
  tags: string[];
  total_minutes: number;
  created_on: string;
  cooking: boolean;
  next_block: KitchenBlock | null;
};

export type KitchenOpenSession = {
  recipe_id: string;
  title: string;
  started_at_millis: number;
  deleted: boolean;
};

export type KitchenRecipeList = {
  open_sessions: KitchenOpenSession[];
  next_block: KitchenBlock | null;
  next_block_recipes: KitchenRecipeSummary[];
  recipes: KitchenRecipeSummary[];
};

export type KitchenNutrition = {
  calories: number;
  protein_grams: number;
  carbs_grams: number;
  fat_grams: number;
  fiber_grams: number | null;
};

export type KitchenIngredient = {
  id: string;
  name: string;
  quantity: string;
  note: string | null;
  optional: boolean;
  need_to_buy: boolean;
  gathered: boolean;
};

export type KitchenIngredientGroup = {
  name: string | null;
  ingredients: KitchenIngredient[];
};

export type KitchenTimerState = "idle" | "running" | "paused" | "done";

export type KitchenTimer = {
  index: number;
  label: string;
  minutes: number;
  state: KitchenTimerState;
  ends_at_millis: number | null;
  remaining_millis: number | null;
  rings_here: boolean;
};

export type KitchenStep = {
  index: number;
  text: string;
  done: boolean;
  timers: KitchenTimer[];
};

export type KitchenSession = {
  id: string;
  servings: number;
  recipe_revision: number;
  started_at_millis: number;
};

export type KitchenPastSession = {
  id: string;
  servings: number;
  recipe_revision: number;
  started_at_millis: number;
  finished_at_millis: number;
  duration_minutes: number;
  notes: string | null;
};

export type KitchenRecipe = {
  id: string;
  revision: number;
  newer_revision: number | null;
  deleted: boolean;
  read_only: boolean;
  title: string;
  summary: string;
  cuisine: string | null;
  tags: string[];
  recipe_servings: number;
  servings: number;
  active_minutes: number;
  total_minutes: number;
  created_on: string;
  nutrition: KitchenNutrition | null;
  groups: KitchenIngredientGroup[];
  shopping: KitchenIngredient[];
  equipment: string[];
  steps: KitchenStep[];
  notes: string[];
  session: KitchenSession | null;
  past_sessions: KitchenPastSession[];
  coming_blocks: KitchenBlock[];
};

export type KitchenPantryItem = {
  id: string;
  name: string;
  category: string;
  amount: string | null;
  use_by: string | null;
  notes: string | null;
  listed_twice: boolean;
};

export type KitchenPantryCategory = {
  name: string;
  items: KitchenPantryItem[];
};

export type KitchenEquipment = {
  id: string;
  name: string;
  notes: string | null;
  listed_twice: boolean;
};

export type KitchenPantry = {
  categories: KitchenPantryCategory[];
  equipment: KitchenEquipment[];
};

export type KitchenPlan = {
  item_id: string;
  occurrence_key: string;
  recipe_id: string;
  title: string;
};

export type KitchenTimerAction = "start" | "pause" | "resume" | "add_minute" | "clear";

export type KitchenSessionChange =
  | { change: "start" }
  | { change: "servings"; servings: number }
  | { change: "gathered"; ingredient: string; gathered: boolean }
  | { change: "step_done"; step: number; done: boolean }
  | { change: "timer"; step: number; timer: number; action: KitchenTimerAction }
  | { change: "finish"; notes: string | null }
  | { change: "discard" };

export type KitchenPantryChange =
  | {
      change: "save_item";
      id: string | null;
      name: string;
      category: string;
      amount: string | null;
      use_by: string | null;
      notes: string | null;
    }
  | { change: "delete_item"; id: string }
  | { change: "save_equipment"; id: string | null; name: string; notes: string | null }
  | { change: "delete_equipment"; id: string };

export type KitchenBackend = {
  recipes: (search: string, zone: string) => Promise<KitchenRecipeList>;
  recipe: (id: string, servings: number | null, zone: string) => Promise<KitchenRecipe>;
  recipeHistory: (id: string) => Promise<AppDocumentRevision[]>;
  recipeRevision: (id: string, revision: number, servings: number | null) => Promise<KitchenRecipe>;
  changeSession: (
    recipeId: string,
    revision: number,
    servings: number,
    change: KitchenSessionChange,
  ) => Promise<void>;
  pantry: () => Promise<KitchenPantry>;
  changePantry: (change: KitchenPantryChange) => Promise<void>;
  plans: () => Promise<KitchenPlan[]>;
  keepDisplayAwake: (on: boolean) => Promise<void>;
};
