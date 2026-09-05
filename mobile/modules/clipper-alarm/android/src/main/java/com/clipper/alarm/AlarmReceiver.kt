package com.clipper.alarm

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Fires when an exact alarm goes off.
 *
 * The system cold-starts the process to deliver this even if the app has never
 * been opened, and — because every component on this path is `directBootAware`
 * — even if the device has rebooted and not yet been unlocked.
 */
class AlarmReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != AlarmIntents.ACTION_FIRE) return
        val appContext = context.applicationContext
        val index = intent.getIntExtra(AlarmIntents.EXTRA_INDEX, -1)

        // Prefer the mirror, fall back to the intent. The two are written
        // together, but an alarm that cannot find its label should still ring:
        // a nameless alarm beats a silent one.
        val planned = AlarmMirror.at(appContext, index)
        val label = planned?.label
            ?: intent.getStringExtra(AlarmIntents.EXTRA_LABEL)
            ?: DEFAULT_LABEL
        val itemId = planned?.itemId ?: intent.getStringExtra(AlarmIntents.EXTRA_ITEM_ID).orEmpty()
        val occurrenceKey = planned?.occurrenceKey
            ?: intent.getStringExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY).orEmpty()

        Log.i(TAG, "Alarm $index fired: $label")
        RingService.start(appContext, label, itemId, occurrenceKey)

        // Roll the horizon forward. Re-arming reads only the device-protected
        // mirror, so it works before unlock, when the schedule itself is still
        // sealed.
        runCatching { AlarmScheduler(appContext).armFromMirror() }
            .onFailure { Log.w(TAG, "Failed to re-arm after firing", it) }
    }

    private companion object {
        const val TAG = "ClipperAlarm"
        const val DEFAULT_LABEL = "Alarm"
    }
}
