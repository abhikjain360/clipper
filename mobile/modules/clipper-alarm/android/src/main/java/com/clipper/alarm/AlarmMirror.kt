package com.clipper.alarm

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONObject

/**
 * One planned alarm, flattened to what the ring screen needs.
 *
 * The label travels with the alarm rather than being looked up. After a reboot
 * the device sits at the lock screen with credential-protected storage sealed,
 * so there is no encrypted store to read a title from. That is exactly when an
 * alarm clock has to work.
 */
data class PlannedAlarm(
    /** The schedule series this belongs to. */
    val itemId: String,
    /** Identifies the occurrence, so a dismissal lands on the right one. */
    val occurrenceKey: String,
    val label: String,
    val fireAtMillis: Long,
    val occurrenceStartMillis: Long,
) {
    fun toJson(): JSONObject = JSONObject().apply {
        put(KEY_ITEM_ID, itemId)
        put(KEY_OCCURRENCE, occurrenceKey)
        put(KEY_LABEL, label)
        put(KEY_FIRE_AT, fireAtMillis)
        put(KEY_START, occurrenceStartMillis)
    }

    companion object {
        private const val KEY_ITEM_ID = "item_id"
        private const val KEY_OCCURRENCE = "occurrence_key"
        private const val KEY_LABEL = "label"
        private const val KEY_FIRE_AT = "fire_at"
        private const val KEY_START = "start"

        fun fromJson(json: JSONObject): PlannedAlarm = PlannedAlarm(
            itemId = json.optString(KEY_ITEM_ID),
            occurrenceKey = json.optString(KEY_OCCURRENCE),
            label = json.optString(KEY_LABEL),
            fireAtMillis = json.optLong(KEY_FIRE_AT),
            occurrenceStartMillis = json.optLong(KEY_START),
        )
    }
}

/**
 * The alarm plan, held in device-protected storage.
 *
 * This is what makes exact alarms work alongside end-to-end encryption. The
 * schedule is ciphertext the app can only read once the user has unlocked and
 * logged in, but an alarm has to survive a reboot at 3am and ring at 7am
 * without either. The mirror holds only the upcoming fire times and their
 * labels: no schedule, no keys. Device-protected storage makes them readable
 * before the first unlock.
 *
 * Writes use `commit` rather than `apply`. This is the fallback, so it has to
 * be on disk before the process is killed, not eventually.
 */
object AlarmMirror {
    private const val PREFS = "clipper_alarm_plan"
    private const val KEY_PLAN = "plan"

    fun save(context: Context, alarms: List<PlannedAlarm>) {
        val array = JSONArray()
        alarms.sortedBy { it.fireAtMillis }.forEach { array.put(it.toJson()) }
        check(prefs(context).edit().putString(KEY_PLAN, array.toString()).commit()) {
            "Could not persist the alarm plan"
        }
    }

    fun load(context: Context): List<PlannedAlarm> = loadOrNull(context).orEmpty()

    /** Null means corrupt storage; an absent/cleared plan is an intentional empty set. */
    fun loadOrNull(context: Context): List<PlannedAlarm>? {
        val raw = prefs(context).getString(KEY_PLAN, null) ?: return emptyList()
        return runCatching {
            val array = JSONArray(raw)
            (0 until array.length()).mapNotNull { index ->
                array.optJSONObject(index)?.let(PlannedAlarm::fromJson)
            }
        }.getOrNull()
    }

    fun clear(context: Context) {
        check(prefs(context).edit().remove(KEY_PLAN).commit()) {
            "Could not clear the alarm plan"
        }
    }

    /**
     * Device-protected storage: readable between boot and unlock, unlike the
     * app's default (credential-protected) storage.
     */
    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext
            .createDeviceProtectedStorageContext()
            .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
