package com.clipper.alarm

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Fires when an exact alarm goes off.
 *
 * The system cold-starts the process to deliver this even if the app has never
 * been opened. Every component on this path is `directBootAware`, so it also
 * arrives when the device has rebooted and nobody has unlocked it yet.
 */
class AlarmReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        val snoozed = intent.action == AlarmIntents.ACTION_SNOOZE_FIRE
        if (!snoozed && intent.action != AlarmIntents.ACTION_FIRE) return
        val appContext = context.applicationContext
        val index = intent.getIntExtra(AlarmIntents.EXTRA_INDEX, -1)

        // A delivery can already be queued when replaceAll cancels its pending
        // intent. Mirror indices are reused, so match the actual occurrence and
        // fire time instead of accidentally ringing the new occupant of a slot.
        val deliveredItem = intent.getStringExtra(AlarmIntents.EXTRA_ITEM_ID).orEmpty()
        val deliveredKey = intent.getStringExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY).orEmpty()
        val deliveredAt = intent.getLongExtra(AlarmIntents.EXTRA_FIRE_AT, -1)
        val generation = intent.getLongExtra(AlarmIntents.EXTRA_GENERATION, -1L)
        val planned = synchronized(AlarmMirror) {
            if (generation != AlarmMirror.generation(appContext)) {
                Log.i(TAG, "Ignoring cancelled alarm delivery")
                return
            }
            if (snoozed) {
                AlarmMirror.takeSnooze(appContext, index, deliveredItem, deliveredKey, deliveredAt)
                    ?: return
            } else {
                val mirror = AlarmMirror.loadOrNull(appContext)
                val alarm = mirror?.firstOrNull {
                    it.itemId == deliveredItem && it.occurrenceKey == deliveredKey &&
                        it.fireAtMillis == deliveredAt
                }
                if (mirror != null && alarm == null) {
                    Log.i(TAG, "Ignoring cancelled or superseded alarm delivery")
                    return
                }
                alarm
            }
        }
        // If the mirror is corrupt, the concrete intent still has enough data
        // to ring. A deliberately cleared mirror, including logout, never does.
        val label = planned?.label
            ?: intent.getStringExtra(AlarmIntents.EXTRA_LABEL)
            ?: DEFAULT_LABEL
        val itemId = planned?.itemId ?: intent.getStringExtra(AlarmIntents.EXTRA_ITEM_ID).orEmpty()
        val occurrenceKey = planned?.occurrenceKey
            ?: intent.getStringExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY).orEmpty()

        Log.i(TAG, "Alarm $index fired: $label")
        val alarm = planned ?: PlannedAlarm(itemId, occurrenceKey, label, deliveredAt,
            intent.getLongExtra(AlarmIntents.EXTRA_START, deliveredAt))
        RingService.start(appContext, alarm, generation)

        // Move the alarm window forward. Re-arming reads only the device-protected
        // mirror, so it works before unlock, when the schedule itself is still
        // sealed.
        runCatching { AlarmScheduler(appContext).armFromMirror() }
            .onFailure { Log.w(TAG, "Failed to re-arm after firing", it) }
        ClipperClockWidget.updateAll(appContext)
    }

    private companion object {
        const val TAG = "ClipperAlarm"
        const val DEFAULT_LABEL = "Alarm"
    }
}
