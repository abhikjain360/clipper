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

data class SnoozedAlarm(val requestCode: Int, val alarm: PlannedAlarm)

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
    private const val KEY_SNOOZES = "snoozes"
    private const val KEY_SNOOZE_CODE = "next_snooze_code"
    private const val KEY_GENERATION = "generation"

    fun save(context: Context, alarms: List<PlannedAlarm>) = synchronized(this) {
        val array = JSONArray()
        alarms.sortedBy { it.fireAtMillis }.forEach { array.put(it.toJson()) }
        check(prefs(context).edit().putString(KEY_PLAN, array.toString()).commit()) {
            "Could not persist the alarm plan"
        }
        ClipperClockWidget.updateAll(context)
    }

    fun load(context: Context): List<PlannedAlarm> = loadOrNull(context).orEmpty()

    /** Null means corrupt storage; an absent/cleared plan is an intentional empty set. */
    fun loadOrNull(context: Context): List<PlannedAlarm>? = synchronized(this) {
        val raw = prefs(context).getString(KEY_PLAN, null) ?: return@synchronized emptyList()
        runCatching {
            val array = JSONArray(raw)
            (0 until array.length()).mapNotNull { index ->
                array.optJSONObject(index)?.let(PlannedAlarm::fromJson)
            }
        }.getOrNull()
    }

    fun clear(context: Context) = synchronized(this) {
        AlarmScheduler(context).cancelAll()
        check(prefs(context).edit().remove(KEY_PLAN).commit()) {
            "Could not clear the alarm plan"
        }
        ClipperClockWidget.updateAll(context)
    }

    fun loadSnoozes(context: Context): List<SnoozedAlarm> = synchronized(this) {
        val raw = prefs(context).getString(KEY_SNOOZES, null) ?: return@synchronized emptyList()
        runCatching {
            val array = JSONArray(raw)
            (0 until array.length()).mapNotNull { index ->
                val json = array.optJSONObject(index) ?: return@mapNotNull null
                val code = json.optInt("request_code", 0)
                if (code >= 0) null else SnoozedAlarm(code, PlannedAlarm.fromJson(json))
            }
        }.getOrDefault(emptyList())
    }

    fun loadUpcoming(context: Context): List<PlannedAlarm> = synchronized(this) {
        load(context) + loadSnoozes(context).map { it.alarm }
    }

    internal fun generation(context: Context): Long = synchronized(this) {
        prefs(context).getLong(KEY_GENERATION, 0L)
    }

    internal fun addSnooze(context: Context, alarm: PlannedAlarm): SnoozedAlarm = synchronized(this) {
        val stored = loadSnoozes(context)
        val usedCodes = stored.map { it.requestCode }.toSet()
        var code = prefs(context).getInt(KEY_SNOOZE_CODE, -1).coerceAtMost(-1)
        while (code in usedCodes) code = nextCode(code)
        val snooze = SnoozedAlarm(code, alarm)
        check(prefs(context).edit()
            .putString(KEY_SNOOZES, snoozesJson(stored + snooze))
            .putInt(KEY_SNOOZE_CODE, nextCode(code))
            .commit()) { "Could not save the snoozed alarm" }
        ClipperClockWidget.updateAll(context)
        snooze
    }

    internal fun takeSnooze(
        context: Context,
        requestCode: Int,
        itemId: String,
        occurrenceKey: String,
        fireAtMillis: Long,
    ): PlannedAlarm? = synchronized(this) {
        val stored = loadSnoozes(context)
        val snooze = stored.firstOrNull {
            it.requestCode == requestCode && it.alarm.itemId == itemId &&
                it.alarm.occurrenceKey == occurrenceKey && it.alarm.fireAtMillis == fireAtMillis
        } ?: return@synchronized null
        removeSnooze(context, snooze)
        snooze.alarm
    }

    internal fun removeSnooze(context: Context, snooze: SnoozedAlarm) = synchronized(this) {
        check(prefs(context).edit()
            .putString(KEY_SNOOZES, snoozesJson(loadSnoozes(context).filter { it != snooze }))
            .commit()) { "Could not remove the snoozed alarm" }
        ClipperClockWidget.updateAll(context)
    }

    internal fun clearSnoozes(context: Context) = synchronized(this) {
        check(prefs(context).edit()
            .remove(KEY_SNOOZES)
            .putLong(KEY_GENERATION, generation(context) + 1L)
            .commit()) { "Could not clear the snoozed alarms" }
        ClipperClockWidget.updateAll(context)
    }

    private fun snoozesJson(alarms: List<SnoozedAlarm>): String = JSONArray().apply {
        alarms.forEach { put(it.alarm.toJson().put("request_code", it.requestCode)) }
    }.toString()

    private fun nextCode(code: Int): Int = if (code == Int.MIN_VALUE) -1 else code - 1

    /**
     * Device-protected storage: readable between boot and unlock, unlike the
     * app's default (credential-protected) storage.
     */
    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext
            .createDeviceProtectedStorageContext()
            .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
