package com.clipper.alarm

import android.annotation.SuppressLint
import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log

/** Intent actions and extras for the alarm path, in one place so no string drifts. */
object AlarmIntents {
    const val ACTION_FIRE = "com.clipper.alarm.action.FIRE"
    const val ACTION_SNOOZE_FIRE = "com.clipper.alarm.action.SNOOZE_FIRE"
    const val ACTION_DISMISS = "com.clipper.alarm.action.DISMISS"
    const val ACTION_SNOOZE = "com.clipper.alarm.action.SNOOZE"
    // Sent by the system after the user grants exact-alarm access. Keeping the
    // literal here lets the receiver remain loadable on API levels that do not
    // expose AlarmManager.ACTION_SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED.
    const val ACTION_EXACT_ALARM_PERMISSION_CHANGED =
        "android.app.action.SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED"

    const val EXTRA_INDEX = "com.clipper.alarm.extra.INDEX"
    const val EXTRA_LABEL = "com.clipper.alarm.extra.LABEL"
    const val EXTRA_ITEM_ID = "com.clipper.alarm.extra.ITEM_ID"
    const val EXTRA_OCCURRENCE_KEY = "com.clipper.alarm.extra.OCCURRENCE_KEY"
    const val EXTRA_FIRE_AT = "com.clipper.alarm.extra.FIRE_AT"
    const val EXTRA_START = "com.clipper.alarm.extra.START"
    const val EXTRA_CAN_SNOOZE = "com.clipper.alarm.extra.CAN_SNOOZE"
    const val EXTRA_GENERATION = "com.clipper.alarm.extra.GENERATION"
}

/**
 * Registers exact alarms with the system.
 *
 * Every alarm is a single one-shot [AlarmManager.setAlarmClock]. Android's
 * repeating alarms are inexact and get batched. `setAlarmClock` is the only
 * API the system delivers on time, because it leaves Doze. A notification
 * carries no such guarantee, which is why this module exists.
 *
 * Recurrence is never computed here. The Rust engine expands the rules and
 * hands down a list of instants, so there is one recurrence implementation in
 * the project rather than two that can drift apart.
 */
class AlarmScheduler(private val context: Context) {

    private val alarmManager: AlarmManager = context.getSystemService(AlarmManager::class.java)

    /**
     * Replace every registered alarm with the nearest [MAX_REGISTERED] of [plan].
     *
     * The plan can cover weeks, but there is no need to hold hundreds of
     * pending intents. Each fire re-arms from the mirror, so the alarm window
     * moves forward on its own. Returns how many were registered.
     */
    fun replaceAll(plan: List<PlannedAlarm>): Int = synchronized(AlarmMirror) {
        cancelPlan()
        AlarmMirror.save(context, plan)
        // Arm from the mirror rather than from `plan`, so the index used as a
        // request code is always the mirror's own index. Sorting in two places
        // would let two alarms sharing a fire time swap positions, and a fired
        // alarm would then look up the wrong label.
        val count = arm(AlarmMirror.load(context))
        armSnoozes()
        count
    }

    /** Register from whatever the mirror currently holds. Safe before unlock. */
    fun armFromMirror(): Int = synchronized(AlarmMirror) {
        arm(AlarmMirror.load(context)) + armSnoozes()
    }

    @SuppressLint("MissingPermission")
    fun snooze(alarm: PlannedAlarm, generation: Long): Unit = synchronized(AlarmMirror) {
        if (!alarm.canSnooze) return@synchronized
        check(generation == AlarmMirror.generation(context)) { "The alarm was cancelled" }
        check(canScheduleExactAlarms()) { "Exact alarms are not permitted" }
        val snooze = AlarmMirror.addSnooze(context,
            alarm.copy(fireAtMillis = System.currentTimeMillis() + SNOOZE_MS))
        try {
            registerSnooze(snooze)
        } catch (error: Throwable) {
            cancelSnooze(snooze)
            AlarmMirror.removeSnooze(context, snooze)
            throw error
        }
    }

    private fun armSnoozes(): Int {
        if (!canScheduleExactAlarms()) return 0
        val snoozes = AlarmMirror.loadSnoozes(context)
        snoozes.forEach(::registerSnooze)
        return snoozes.size
    }

    @SuppressLint("MissingPermission")
    private fun registerSnooze(snooze: SnoozedAlarm) {
        val fireAt = maxOf(snooze.alarm.fireAtMillis, System.currentTimeMillis() + 1_000L)
        val info = AlarmManager.AlarmClockInfo(fireAt, showIntent())
        alarmManager.setAlarmClock(info,
            firePendingIntent(snooze.requestCode, snooze.alarm, AlarmIntents.ACTION_SNOOZE_FIRE))
    }

    /** [plan] must be in mirror order. See [replaceAll]. */
    @SuppressLint("MissingPermission")
    private fun arm(plan: List<PlannedAlarm>): Int {
        if (!canScheduleExactAlarms()) {
            // Fail loudly rather than fall back to an inexact alarm, which
            // would look like it worked and then ring late.
            Log.w(TAG, "Exact alarms are not permitted; nothing scheduled")
            return 0
        }
        val now = System.currentTimeMillis()
        var registered = 0
        for ((index, alarm) in plan.withIndex()) {
            if (registered >= MAX_REGISTERED) break
            // An entry already in the past stays in the mirror and is left
            // unarmed. Rewriting the plan on every fire would mean writing
            // from a broadcast receiver that may run before unlock.
            if (alarm.fireAtMillis <= now) continue
            val info = AlarmManager.AlarmClockInfo(alarm.fireAtMillis, showIntent())
            alarmManager.setAlarmClock(info, firePendingIntent(index, alarm))
            registered++
        }
        Log.i(TAG, "Registered $registered of ${plan.size} planned alarms")
        return registered
    }

    fun cancelAll(): Unit = synchronized(AlarmMirror) {
        cancelPlan()
        AlarmMirror.loadSnoozes(context).forEach(::cancelSnooze)
        AlarmMirror.clearSnoozes(context)
    }

    private fun cancelSnooze(snooze: SnoozedAlarm) {
        pendingIntentOrNull(snooze.requestCode, AlarmIntents.ACTION_SNOOZE_FIRE)?.let {
            alarmManager.cancel(it)
            it.cancel()
        }
    }

    private fun cancelPlan() {
        // Sweep the whole index space the previous plan could have used, not
        // just MAX_REGISTERED of it. A request code is the alarm's index in
        // the mirror, and a past entry is skipped rather than armed. So a plan
        // whose first entries have expired arms indices well beyond
        // MAX_REGISTERED, and a narrower sweep would strand them.
        val span = maxOf(MAX_REGISTERED, AlarmMirror.load(context).size)
        for (index in 0 until span) {
            pendingIntentOrNull(index)?.let {
                alarmManager.cancel(it)
                it.cancel()
            }
        }
    }

    fun canScheduleExactAlarms(): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            alarmManager.canScheduleExactAlarms()
        } else {
            true
        }

    private fun firePendingIntent(
        index: Int,
        alarm: PlannedAlarm,
        fireAction: String = AlarmIntents.ACTION_FIRE,
    ): PendingIntent {
        val intent = Intent(context, AlarmReceiver::class.java).apply {
            action = fireAction
            putExtra(AlarmIntents.EXTRA_INDEX, index)
            // Carried in the intent as well as the mirror so a fire can ring
            // even if the mirror is somehow unreadable.
            putExtra(AlarmIntents.EXTRA_LABEL, alarm.label)
            putExtra(AlarmIntents.EXTRA_ITEM_ID, alarm.itemId)
            putExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY, alarm.occurrenceKey)
            putExtra(AlarmIntents.EXTRA_FIRE_AT, alarm.fireAtMillis)
            putExtra(AlarmIntents.EXTRA_START, alarm.occurrenceStartMillis)
            putExtra(AlarmIntents.EXTRA_CAN_SNOOZE, alarm.canSnooze)
            putExtra(AlarmIntents.EXTRA_GENERATION, AlarmMirror.generation(context))
        }
        return PendingIntent.getBroadcast(
            context,
            index,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun pendingIntentOrNull(
        index: Int,
        fireAction: String = AlarmIntents.ACTION_FIRE,
    ): PendingIntent? {
        // PendingIntent matching ignores extras; component plus action is enough.
        val intent = Intent(context, AlarmReceiver::class.java).apply {
            action = fireAction
        }
        return PendingIntent.getBroadcast(
            context,
            index,
            intent,
            PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    /** Opens the app when the user taps the system's next-alarm chip. */
    private fun showIntent(): PendingIntent {
        val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
            ?: Intent()
        return PendingIntent.getActivity(context, 0, launch, PendingIntent.FLAG_IMMUTABLE)
    }

    companion object {
        private const val TAG = "ClipperAlarm"
        internal const val SNOOZE_MS = 10 * 60 * 1000L

        /**
         * How many alarms are held in the system registry at once. Each fire
         * re-arms from the mirror, so this bounds pending intents without
         * bounding the alarm window.
         */
        const val MAX_REGISTERED = 24
    }
}
