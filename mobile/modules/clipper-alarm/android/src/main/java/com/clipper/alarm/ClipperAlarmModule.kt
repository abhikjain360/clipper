package com.clipper.alarm

import android.content.Intent
import android.net.Uri
import android.provider.Settings
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import org.json.JSONArray

/**
 * The JS surface of the alarm layer.
 *
 * Intentionally small. Everything about *when* an alarm rings is decided in
 * Rust and arrives here as instants; this module only registers them, reports
 * whether the OS will honour exact alarms, and can send the owner to the
 * setting when it will not.
 *
 * The plan crosses as a JSON string rather than a list of typed records.
 * Expo's argument marshalling relies on reified generics that do not survive a
 * `List<Record>` parameter here — it fails at runtime, not compile time — and
 * the plan is already JSON on both sides, so a string costs nothing and removes
 * a whole class of binding fragility.
 */
class ClipperAlarmModule : Module() {

    override fun definition() = ModuleDefinition {
        Name("ClipperAlarm")

        /**
         * Replace the entire registered set with the plan in [planJson].
         *
         * Returns how many were armed, which is at most
         * `AlarmScheduler.MAX_REGISTERED` — each fire rolls the horizon forward
         * from the device-protected mirror.
         */
        Function("setAlarms") { planJson: String ->
            AlarmScheduler(context).replaceAll(parsePlan(planJson))
        }

        Function("cancelAll") {
            AlarmScheduler(context).cancelAll()
            AlarmMirror.clear(context)
        }

        /**
         * Whether the OS will deliver exact alarms. False means every alarm
         * would be late, so the UI should say so rather than pretend.
         */
        Function("canScheduleExactAlarms") {
            AlarmScheduler(context).canScheduleExactAlarms()
        }

        /** Open the system screen where exact alarms are granted. */
        Function("openExactAlarmSettings") {
            val intent = Intent(Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM).apply {
                data = Uri.fromParts("package", context.packageName, null)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            runCatching { context.startActivity(intent) }.isSuccess
        }

        /** What the device-protected mirror currently holds, for diagnostics. */
        Function("plannedCount") {
            AlarmMirror.load(context).size
        }

        /**
         * Ring immediately. Exists so the alarm path can be exercised end to
         * end — permissions, foreground service, full-screen intent — without
         * waiting for a real alarm time.
         */
        Function("ringNow") { label: String ->
            RingService.start(context, label, "", "")
        }

        Function("dismiss") {
            RingService.dismiss(context)
        }
    }

    /**
     * A malformed entry is skipped rather than failing the whole plan: one bad
     * alarm should not silence the rest.
     */
    private fun parsePlan(planJson: String): List<PlannedAlarm> = runCatching {
        val array = JSONArray(planJson)
        (0 until array.length()).mapNotNull { index ->
            array.optJSONObject(index)?.let(PlannedAlarm::fromJson)
        }
    }.getOrDefault(emptyList())

    private val context
        get() = requireNotNull(appContext.reactContext) {
            "ClipperAlarm needs a React context"
        }
}
