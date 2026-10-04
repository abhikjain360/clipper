# Kitchen

This document specifies the kitchen, the second app on [app data](app-data.md)
after the gym logger. Recipes, the pantry, the equipment list, cooking
sessions and meal plans are collections. Claude plans meals and writes recipes
through the desktop daemon. The Android app and the Mac app show the recipes
and run the cooking screen. The browser client has no kitchen, because it does
not sync app data.

## Terms

- **Recipe**: a dish Claude wrote for a number of servings, with ingredients,
  steps and step timers.
- **Step timer**: a countdown attached to one step, such as "simmer, 20
  minutes".
- **Cooking session**: one time a recipe is cooked, from its start until it is
  finished or discarded.
- **Cooking block**: a schedule block set aside for batch cooking, today
  Wednesday 05:00 and Sunday 08:00. See [schedule-model.md](schedule-model.md).
- **Plan**: a recipe assigned to one occurrence of a block.
- **Cooking workspace**: the local directory, outside Clipper, where Claude
  plans meals. It holds the cook skill, its `AGENTS.md` and the owner's
  `kitchen/profile.md`.

## Collections

Field names inside values are snake_case, like other Clipper types. The Rust
types, their checks and the collection names are in `crates/kitchen`. Each type
gives lists and flags a default, so a writer can leave out an empty list or a
flag at its default. A value with a field the type does not have is refused,
so a misspelt field cannot be dropped without notice.

A date is accepted only as `YYYY-MM-DD` and a time only as RFC 3339 with a `T`
between date and time, such as `2026-10-07T18:30:00Z` or with an offset. The
stored value is the Rust type's own encoding of the value, not the text that
was written:

- every list and flag is present, at its default when it was left out;
- an optional field that was left out or written as `null` is absent;
- a time is in UTC with milliseconds, such as `2026-10-07T18:30:00.000Z`, so
  stored dates and times sort correctly as text.

### kitchen.recipes (document collection)

One recipe per document. Only Claude writes recipes, from the Mac; the screens
read them. Every revision is kept, so `clipper data history` shows how a
recipe changed. The value has these fields:

- `title`, `summary`, optional `cuisine`, `tags`, `servings`,
  `active_minutes`, `total_minutes`, `equipment` (display names), `notes` and
  `created_on`.
- `ingredients`: each with `id`, `name`, optional `amount`, `unit`, `note` and
  `group`, and the flags `scales` (default true), `optional` and `need_to_buy`
  (default false).
- `steps`: each with `text`, which refers to ingredients as `{id}`, and
  `timers`, each a `label` and `minutes`.
- optional `nutrition_per_serving`: `calories`, `protein_grams`,
  `carbs_grams`, `fat_grams` and optional `fiber_grams`.

A value decodes only if:

- ingredient ids are lowercase words joined by hyphens, unique in the recipe;
- every `{id}` in a step names an ingredient;
- a unit comes with an amount, and amounts, minutes and servings are positive;
- there is at least one ingredient, one step, and one timer in every step;
- active minutes do not exceed total minutes;
- `created_on` is a date and no text field is empty.

`kitchen.recipes` has no indexed fields.

Sessions and plans refer to a recipe by its document id. If two writes to one
recipe race, the later one fails with a revision conflict; Claude reads the
recipe again and reapplies its change.

### kitchen.pantry and kitchen.equipment (row collections, last write wins)

A `kitchen.pantry` row is one ingredient on hand: `name`, `category`, optional
`amount` (free text, such as "250 g"), optional `use_by` date and optional
`notes`, where Claude records nutrition label values. A `kitchen.equipment`
row is one piece of equipment: `name` and optional `notes`, such as the hob
calibration. Both are indexed by `name`.

Every item is its own row, so a pantry edit on the phone and one by Claude
cannot overwrite each other. A row check cannot see other rows, so the writers
keep names unique: the pantry screen and Claude look a name up before adding
it, and the pantry screen marks a name that appears twice, which two offline
devices can still cause.

### kitchen.sessions (row collection, last write wins)

One cooking session per row:

- `recipe` (document id) and `recipe_revision`, the recipe revision the
  session started from, so step indexes and step times refer to the steps as
  they were then;
- `servings`, `started_at` and, once finished, `finished_at`;
- `steps_done`: the index and time of each ticked step, as `step` and
  `done_at`; a step appears at most once;
- `gathered`: the ids of ticked ingredients, each at most once;
- `timers`: the step timers that are set, each with its `step` and `timer`
  index, the `device_id` it rings on, and `ends_at` while running or
  `remaining_ms` while paused, never both. A step and timer pair appears at
  most once. A running timer whose end has passed is done;
- optional `notes` on how it went.

`recipe` is a UUIDv7, `recipe_revision` and `servings` are at least 1, and
`finished_at` is not before `started_at`.

Indexed by `recipe`. The checklist and the timers are part of the session, so
they survive a restart, show on every device and start empty for each cook.
One person cooks on one device at a time, so last write wins is enough.

### kitchen.plans (row collection, last write wins)

One row per planned recipe: `recipe` (document id) and `block`, an object with
the `item_id` and `occurrence_key` of a block occurrence as `clipper schedule
occurrences` returns them. An occurrence keeps its key when the block is moved,
so a plan follows its block. Indexed by `recipe`. Only Claude writes plans.

### Outside Clipper

The owner's cooking profile, `kitchen/profile.md` in the cooking workspace, is
a local file: it is not in git and not in Clipper. Only Claude reads it, and no
screen shows it. The profile does not record body weight; Claude reads the
latest weighing from `gym.body_weight`.

## How Claude works with it

Claude reaches the kitchen through the desktop daemon (see "Agent access" in
[app-data.md](app-data.md)), so Clipper must be running and signed in on the
Mac. Writing a recipe also needs the server.

- Reading: `clipper data query` with one SQL statement, such as the pantry by
  nearest use-by date, the titles and cuisines of existing recipes, or one
  recipe's sessions and their step times.
- Writing: `clipper data write` takes a collection, an id and the value on
  stdin, or a delete. Without an id it creates a record and prints the new id.
  To change a recipe, Claude queries its value and revision, edits the value
  in a scratch file and writes it back under the same id with
  `--revision` set to the revision it read.
- A refused value comes back with the field path and the rule, for example
  `steps[3].text: {onion} does not match any ingredient id`. List indexes in a
  path count from 0. Claude fixes it and writes again.
- Checks across rows are queries in the cook skill, run after pantry edits:
  names listed twice, and sessions or plans whose recipe no longer exists.
- The newest `written_at` in `kitchen.pantry` is when the pantry was last
  updated, which tells Claude which Knuspr orders to add.
- To plan meals, Claude lists the coming cooking blocks with
  `clipper schedule occurrences` and writes a plan for each recipe.
- To learn from a session, Claude compares its step times with the recipe's
  timers. When the session started from an older recipe revision, Claude reads
  that revision with `clipper data history`.

Before handing over a recipe, Claude runs the cook skill's checks: nutrition
from real product labels, the day's salt and the EU upper limits, equipment,
food safety and an independent review. Claude then names the recipe by title;
it appears on every device once the Mac has synced.

## Screens

Android has a Kitchen tab beside Schedule and Alarms, and the Mac app a
Kitchen destination in its navigation. Both read the local tables, so the
kitchen opens offline. The browser client shows a note instead, because it
does not sync app data.

Scaling, quantity formatting, cooking session changes and timer states live
in Rust: `crates/kitchen` holds the rules and the client engine builds the
views both apps render, so the screens only display them and send changes.
Quantities are formatted like this:

- an ingredient with `scales` false keeps its amount at every servings count;
- grams and millilitres from 1000 are shown in kilograms and litres;
- other amounts show a whole number and the nearest of ¼, ⅓, ½, ⅔ and ¾ when
  they are close to one, and up to two decimals otherwise;
- units are shown as the recipe writes them;
- a step names each referenced ingredient with its scaled quantity, for
  example `Slice the Onion (3 pieces)`.

- **Recipe list**: newest first, searchable by title, summary, cuisine and
  tags; every search word must appear in one of them. A recipe with a coming
  plan shows the block's day and time; coming plans are looked up in the next
  60 days of the schedule.
- **Recipe page**: summary, times, nutrition per serving, ingredients by
  group, a shopping list of `need_to_buy` ingredients, equipment, steps with
  scaled quantities in the text, notes, and past sessions with their duration
  and notes.
- **Servings scaler**: without a session it changes only the page. Once a
  session is open it shows and changes the session's servings.
- **Cooking session**: Start cooking, ticking an ingredient or a step, or
  starting a timer opens a session from the recipe's current revision. The page shows the
  newest unfinished session of the recipe. Finish asks "How was it?" and saves
  the notes; Discard deletes the row.
- **Step timers**: each can be started, paused, resumed, extended by one
  minute and cleared. Starting one records this device as the one it rings on;
  resuming keeps that device. Pausing stores the time left. A minute added to
  a timer that has ended starts it again with one minute. Every device shows
  the countdown.
- **History**: the recipe page lists the recipe's revisions and opens an
  earlier one read-only, without session controls. Both need the server.
- **Pantry**: items by category with the nearest use-by first; add, edit and
  delete, with categories suggested from existing ones. Equipment is a second
  list on the same screen.
- **Screen awake**: while a recipe page is open, Android keeps the screen on
  and the Mac app holds a power assertion that stops the display sleeping
  (`caffeinate -d`, tied to the app's process).

### Step timers as alarms

A step timer rings through the alarm path in
[schedule-model.md](schedule-model.md), so a phone rings with the screen off,
the app closed or Do Not Disturb on. It rings only on the device that started
it.

- The alarm plan includes one alarm for each running timer in an unfinished
  session whose device is this one, labelled with the timer label and the
  recipe title, for example `Simmer · French onion soup`. Its item is the
  session row id and its occurrence is `timer:<step>:<timer>`. Alarms carry
  `can_snooze`, which is false for step timers.
- Android sends a new alarm list whenever `kitchen.sessions` changes, locally
  or by sync, besides the existing triggers: every app-data change, local or
  received, counts as a change of the app state. A step timer's ring screen
  offers Dismiss and no snooze; more time is added on the recipe page.
- On a Mac, a step timer is delivered like an alarm targeted at that Mac: a
  notification with sound while Clipper is running.
- Ticking the step, finishing or discarding the session clears its timers and
  their alarms.

## Schedule

Cooking blocks are ordinary schedule blocks. An occurrence with plans shows
the planned recipe titles, and tapping one opens its recipe page. The Kitchen
tab starts with the next planned block and its recipes. Plans for past blocks
stay as a record. Cooking sessions do not start or stop the block's timer.

## Import

The recipes and pantry of the earlier cooking app are imported once. Claude
runs the import from the cooking workspace, with Clipper running on the Mac,
using `jq` and `clipper`. No import code is committed.

1. Check that the kitchen collections are empty, so the import cannot run
   twice.
2. Rename the fields of each `kitchen/recipes/*.json` file to snake_case and
   write it to `kitchen.recipes` as a new document.
3. Write each item of `kitchen/pantry.json` to `kitchen.pantry` and each
   equipment entry to `kitchen.equipment`.
4. Skip `kitchen/cooking-log.json`.
5. Compare counts (3 recipes, 87 pantry items, 27 pieces of equipment), run
   the cross-row checks, and open each recipe on the phone.
6. In `kitchen/profile.md`, name the favourite recipe by title instead of by
   file path, and remove the weight line.
7. Rewrite `AGENTS.md` and the cook skill for Clipper, then delete the earlier
   app's code, its Node project files and everything in `kitchen/` except
   `profile.md`.
