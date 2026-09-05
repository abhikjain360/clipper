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
    const val ACTION_DISMISS = "com.clipper.alarm.action.DISMISS"
    const val ACTION_SNOOZE = "com.clipper.alarm.action.SNOOZE"

    const val EXTRA_INDEX = "com.clipper.alarm.extra.INDEX"
    const val EXTRA_LABEL = "com.clipper.alarm.extra.LABEL"
    const val EXTRA_ITEM_ID = "com.clipper.alarm.extra.ITEM_ID"
    const val EXTRA_OCCURRENCE_KEY = "com.clipper.alarm.extra.OCCURRENCE_KEY"
    const val EXTRA_FIRE_AT = "com.clipper.alarm.extra.FIRE_AT"
}

/**
 * Registers exact alarms with the system.
 *
 * Every alarm is a single one-shot [AlarmManager.setAlarmClock]. Android's
 * repeating alarms are inexact and get batched; `setAlarmClock` is the only API
 * the system guarantees to deliver on time, because it leaves Doze. That
 * guarantee is the entire reason this code exists rather than a notification.
 *
 * Recurrence is never computed here. The Rust engine expands the rules and hands
 * down a list of instants — see `crates/schedule/src/alarm.rs` for why there is
 * deliberately only one recurrence implementation in this project.
 */
class AlarmScheduler(private val context: Context) {

    private val alarmManager: AlarmManager = context.getSystemService(AlarmManager::class.java)

    /**
     * Replace every registered alarm with the nearest [MAX_REGISTERED] of [plan].
     *
     * The plan can cover weeks, but holding hundreds of pending intents is both
     * wasteful and unnecessary: each fire re-arms from the mirror, so the
     * horizon rolls forward on its own. Returns how many were actually
     * registered.
     */
    fun replaceAll(plan: List<PlannedAlarm>): Int {
        cancelAll()
        AlarmMirror.save(context, plan)
        // Arm from the mirror rather than from `plan`, so the index used as a
        // request code is always the mirror's own index. Sorting in two places
        // would let two alarms sharing a fire time swap positions, and a fired
        // alarm would then look up the wrong label.
        return armFromMirror()
    }

    /** Register from whatever the mirror currently holds. Safe before unlock. */
    fun armFromMirror(): Int = arm(AlarmMirror.load(context))

    /** [plan] must be in mirror order — see [replaceAll]. */
    @SuppressLint("MissingPermission")
    private fun arm(plan: List<PlannedAlarm>): Int {
        if (!canScheduleExactAlarms()) {
            // Falling back to an inexact alarm would be worse than failing
            // loudly: it looks like it worked and then rings late.
            Log.w(TAG, "Exact alarms are not permitted; nothing scheduled")
            return 0
        }
        val now = System.currentTimeMillis()
        var registered = 0
        for ((index, alarm) in plan.withIndex()) {
            if (registered >= MAX_REGISTERED) break
            // Already-past entries stay in the mirror — they are simply not
            // armed. Rewriting the plan on every fire would mean a write from a
            // broadcast receiver that may be running before unlock.
            if (alarm.fireAtMillis <= now) continue
            val info = AlarmManager.AlarmClockInfo(alarm.fireAtMillis, showIntent())
            alarmManager.setAlarmClock(info, firePendingIntent(index, alarm))
            registered++
        }
        Log.i(TAG, "Registered $registered of ${plan.size} planned alarms")
        return registered
    }

    fun cancelAll() {
        // Cancel the whole request-code range rather than only what the current
        // mirror lists: a plan that shrank must not leave orphaned alarms armed.
        for (index in 0 until MAX_REGISTERED) {
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

    private fun firePendingIntent(index: Int, alarm: PlannedAlarm): PendingIntent {
        val intent = Intent(context, AlarmReceiver::class.java).apply {
            action = AlarmIntents.ACTION_FIRE
            putExtra(AlarmIntents.EXTRA_INDEX, index)
            // Carried in the intent as well as the mirror so a fire can ring
            // even if the mirror is somehow unreadable.
            putExtra(AlarmIntents.EXTRA_LABEL, alarm.label)
            putExtra(AlarmIntents.EXTRA_ITEM_ID, alarm.itemId)
            putExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY, alarm.occurrenceKey)
            putExtra(AlarmIntents.EXTRA_FIRE_AT, alarm.fireAtMillis)
        }
        return PendingIntent.getBroadcast(
            context,
            index,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun pendingIntentOrNull(index: Int): PendingIntent? {
        // PendingIntent matching ignores extras; component plus action is enough.
        val intent = Intent(context, AlarmReceiver::class.java).apply {
            action = AlarmIntents.ACTION_FIRE
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

        /**
         * How many alarms are held in the system registry at once. Each fire
         * re-arms from the mirror, so this bounds pending intents without
         * bounding the horizon.
         */
        const val MAX_REGISTERED = 24
    }
}
