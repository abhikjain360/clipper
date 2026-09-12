package com.clipper.alarm

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Re-registers alarms after anything that clears the system's alarm registry:
 * a reboot, an app update, or a clock or timezone change.
 *
 * `LOCKED_BOOT_COMPLETED` is the one that matters. It arrives while the device
 * is still at the lock screen, before credential-protected storage is
 * available, so re-arming reads the device-protected mirror and nothing else.
 * Waiting for `BOOT_COMPLETED` would leave a device that rebooted overnight
 * with no alarms registered until someone unlocks it.
 *
 * On Xiaomi and HyperOS this only runs if the app has not been force-stopped
 * and Autostart is allowed. The user has to grant that once, by hand. No code
 * here can stand in for it.
 */
class BootReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_LOCKED_BOOT_COMPLETED,
            Intent.ACTION_BOOT_COMPLETED,
            Intent.ACTION_MY_PACKAGE_REPLACED,
            Intent.ACTION_TIME_CHANGED,
            Intent.ACTION_TIMEZONE_CHANGED,
            AlarmIntents.ACTION_EXACT_ALARM_PERMISSION_CHANGED,
            -> {
                val pending = goAsync()
                try {
                    val count = AlarmScheduler(context.applicationContext).armFromMirror()
                    Log.i(TAG, "Re-armed $count alarms after ${intent.action}")
                } catch (error: Throwable) {
                    Log.e(TAG, "Failed to re-arm alarms after ${intent.action}", error)
                } finally {
                    pending.finish()
                }
            }
        }
    }

    private companion object {
        const val TAG = "ClipperAlarm"
    }
}
