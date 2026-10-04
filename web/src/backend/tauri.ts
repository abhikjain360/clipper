import { invoke } from "@tauri-apps/api/core";
import type {
    ActualView,
    AppDocumentRevision,
    AppState,
    ClipboardPayload,
    ClipperBackend,
    CollabItem,
    DeviceInfo,
    GymBodyWeight,
    GymExercise,
    GymMuscleFatigue,
    GymMuscleInfo,
    GymOneRepMax,
    GymPlannedExercise,
    GymSession,
    GymSessionSummary,
    GymStarterLibrary,
    GymTemplate,
    GymWeeklyBodyWeight,
    IngestReport,
    KitchenPantry,
    KitchenPlan,
    KitchenRecipe,
    KitchenRecipeList,
    LogoutOutcome,
    OccurrenceView,
    ScheduleItem,
} from "@clipper/shared";

type RawClipboardPayload = {
    mimeType: string;
    bytes: number[] | ArrayBuffer | Uint8Array;
    text: string | null;
};

export function tauriBackend(): ClipperBackend {
    return {
        connect: () => invoke<void>("connect"),
        defaultServerUrl: () => invoke<string>("default_server_url"),
        login: (passphrase, username, deviceName, serverUrl) =>
            invoke<void>("login", { passphrase, username, deviceName, serverUrl }),
        register: (accessKey, username, passphrase, deviceName, serverUrl) =>
            invoke<string>("register", { accessKey, username, passphrase, deviceName, serverUrl }),
        logout: (cancelRunningWork) => invoke<LogoutOutcome>("logout", { cancelRunningWork }),
        getState: () => invoke<AppState>("get_state"),
        stateVersion: () => invoke<number>("state_version"),
        waitForStateChange: (seenVersion) =>
            invoke<number>("wait_for_state_change", { seenVersion }),
        refresh: () => invoke<void>("refresh"),
        sendClipboardText: (text) => invoke<string>("send_clipboard_text", { text }),
        sendCurrentClipboardText: () => invoke<string | null>("send_current_clipboard_text"),
        sendClipboardPayload: (mimeType, bytes) =>
            invoke<string>("send_clipboard_payload", { mimeType, bytes: [...bytes] }),
        clipboardPayload: async (id) =>
            normalizeClipboardPayload(
                await invoke<RawClipboardPayload>("clipboard_payload", { id }),
            ),
        writeClipboardItemText: (id) => invoke<void>("write_clipboard_item_text", { id }),
        uploadFileBytes: (filename, mimeType, bytes) =>
            invoke<string>("upload_file_bytes", { filename, mimeType, bytes: [...bytes] }),
        uploadFileFromDialog: () => invoke<string | null>("upload_file_from_dialog"),
        downloadFileBytes: async (fileId) =>
            bytesFrom(await invoke<number[]>("download_file_bytes", { fileId })),
        downloadFileToDialog: (fileId, defaultFilename) =>
            invoke<boolean>("download_file_to_dialog", { fileId, defaultFilename }),
        deleteFile: (fileId) => invoke<void>("delete_file", { fileId }),
        startActual: (planContext) =>
            invoke<string>("start_actual", {
                planContext: planContext ?? null,
            }),
        stopActual: (objectId) => invoke<string>("stop_actual", { objectId }),
        actualsBetween: (from, to) => invoke<ActualView[]>("actuals_between", { from, to }),
        addCalendarSource: (name, url) => invoke<string>("add_calendar_source", { name, url }),
        syncCalendarSource: (objectId) =>
            invoke<IngestReport>("sync_calendar_source", { objectId }),
        setCalendarSourceAlarms: (objectId, alarmsOn) =>
            invoke<void>("set_calendar_source_alarms", { objectId, alarmsOn }),
        setCalendarSourceAlarmLead: (objectId, minutes) =>
            invoke<void>("set_calendar_source_alarm_lead", { objectId, minutes }),
        setCalendarSourceTargetDevice: (objectId, targetDevice) =>
            invoke<void>("set_calendar_source_target_device", { objectId, targetDevice }),
        createScheduleItem: (item: ScheduleItem) =>
            invoke<string>("create_schedule_item", { item }),
        updateScheduleItem: (objectId, item, expectedRevision) =>
            invoke<string>("update_schedule_item", { objectId, item, expectedRevision }),
        deleteScheduleObject: (objectId) => invoke<void>("delete_schedule_object", { objectId }),
        expandSchedule: (from, to, observerZone) =>
            invoke<OccurrenceView[]>("expand_schedule", { from, to, observerZone }),
        queryAppData: (sql) => invoke("query_app_data", { sql }),
        writeAppData: (collection, rowId, write) =>
            invoke<string>("write_app_data", { collection, rowId, write }),
        createCollabDoc: () => invoke<CollabItem>("create_collab_doc"),
        deleteCollabDoc: (objectId) => invoke<void>("delete_collab_doc", { objectId }),
        renameCollabDoc: (objectId, title) =>
            invoke<CollabItem>("rename_collab_doc", { objectId, title }),
        getCollabDocMeta: (objectId) => invoke<CollabItem>("get_collab_doc_meta", { objectId }),
        listDevices: () => invoke<DeviceInfo[]>("list_devices"),
        removeDevice: (deviceId) => invoke<void>("remove_device", { deviceId }),
        kitchen: {
            recipes: (search, zone) =>
                invoke<KitchenRecipeList>("kitchen_recipes", { search, zone }),
            recipe: (id, servings, zone) =>
                invoke<KitchenRecipe>("kitchen_recipe", { id, servings, zone }),
            recipeHistory: (id) => invoke<AppDocumentRevision[]>("kitchen_recipe_history", { id }),
            recipeRevision: (id, revision, servings) =>
                invoke<KitchenRecipe>("kitchen_recipe_revision", { id, revision, servings }),
            changeSession: (recipeId, revision, servings, change) =>
                invoke<void>("kitchen_change_session", { recipeId, revision, servings, change }),
            pantry: () => invoke<KitchenPantry>("kitchen_pantry"),
            changePantry: (change) => invoke<void>("kitchen_change_pantry", { change }),
            plans: () => invoke<KitchenPlan[]>("kitchen_plans"),
            keepDisplayAwake: (on) => invoke<void>("keep_display_awake", { on }),
        },
        gym: {
            muscles: () => invoke<GymMuscleInfo[]>("gym_muscles"),
            moveTemplateExercise: (exercises, from, to) =>
                invoke<GymPlannedExercise[]>("gym_move_template_exercise", { exercises, from, to }),
            seedStarterLibrary: () => invoke<GymStarterLibrary>("gym_seed_starter_library"),
            exercises: () => invoke<GymExercise[]>("gym_exercises"),
            templates: () => invoke<GymTemplate[]>("gym_templates"),
            openSession: () => invoke<GymSession | null>("gym_open_session"),
            session: (sessionId) => invoke<GymSession>("gym_session", { sessionId }),
            sessions: () => invoke<GymSessionSummary[]>("gym_sessions"),
            bodyWeights: () => invoke<GymBodyWeight[]>("gym_body_weights"),
            weeklyBodyWeight: (zone) =>
                invoke<GymWeeklyBodyWeight[]>("gym_weekly_body_weight", { zone }),
            oneRepMaxProgress: (exerciseId) =>
                invoke<GymOneRepMax[]>("gym_one_rep_max_progress", { exerciseId }),
            fatigue: () => invoke<GymMuscleFatigue[]>("gym_fatigue"),
            change: (change) => invoke<void>("gym_change", { change }),
            scheduleRestEnd: (endsAtMillis, title, body) =>
                invoke<void>("gym_schedule_rest_end", { endsAtMillis, title, body }),
            cancelRestEnd: () => invoke<void>("gym_cancel_rest_end"),
        },
        // Browser-only session resume. The desktop daemon owns credentials and
        // survives webview reloads, so these are never invoked under Tauri; they
        // exist only to satisfy the shared backend contract.
        resume: () => Promise.reject(new Error("Session resume is not used under Tauri")),
        sessionResumeMaterial: () => Promise.resolve(null),
    };
}

function normalizeClipboardPayload(raw: RawClipboardPayload): ClipboardPayload {
    return {
        mimeType: raw.mimeType,
        bytes: bytesFrom(raw.bytes),
        text: raw.text,
    };
}

function bytesFrom(value: number[] | ArrayBuffer | Uint8Array | unknown): Uint8Array {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (Array.isArray(value)) return Uint8Array.from(value);
    return Uint8Array.from([]);
}
