package com.clipper.alarm

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log

/**
 * Holds a ringing alarm alive.
 *
 * A foreground service rather than an activity alone, because an activity can
 * be swiped away or never shown while the sound must keep going until it is
 * deliberately dismissed. The `mediaPlayback` type is what permits audio from
 * the background; the notification it posts carries a full-screen intent, which
 * is how the ring screen appears over the lock screen.
 *
 * A wake lock guards the gap between the alarm firing and the screen coming on.
 * Without it the device can return to sleep mid-start on some vendors.
 */
class RingService : Service() {

    private var ringer: Ringer? = null
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            AlarmIntents.ACTION_DISMISS -> {
                stopRinging()
                return START_NOT_STICKY
            }
        }

        val label = intent?.getStringExtra(AlarmIntents.EXTRA_LABEL) ?: DEFAULT_LABEL
        val itemId = intent?.getStringExtra(AlarmIntents.EXTRA_ITEM_ID).orEmpty()
        val occurrenceKey = intent?.getStringExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY).orEmpty()

        acquireWakeLock()
        startForegroundWithNotification(label, itemId, occurrenceKey)

        if (ringer == null) {
            ringer = Ringer(this).also { it.start(vibrate = true) }
        }

        startActivity(
            RingActivity.intent(this, label, itemId, occurrenceKey)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )

        // START_STICKY: if the system kills this under memory pressure while an
        // alarm is ringing, bringing it back is the right recovery.
        return START_STICKY
    }

    override fun onDestroy() {
        stopRinging()
        super.onDestroy()
    }

    private fun stopRinging() {
        ringer?.stop()
        ringer = null
        wakeLock?.let { if (it.isHeld) runCatching { it.release() } }
        wakeLock = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun acquireWakeLock() {
        if (wakeLock != null) return
        val power = getSystemService(PowerManager::class.java) ?: return
        wakeLock = power
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "clipper:alarm")
            .apply { runCatching { acquire(WAKE_LOCK_TIMEOUT_MS) } }
    }

    private fun startForegroundWithNotification(
        label: String,
        itemId: String,
        occurrenceKey: String,
    ) {
        ensureChannel(this)

        val fullScreen = PendingIntent.getActivity(
            this,
            0,
            RingActivity.intent(this, label, itemId, occurrenceKey),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val dismiss = PendingIntent.getService(
            this,
            1,
            Intent(this, RingService::class.java).setAction(AlarmIntents.ACTION_DISMISS),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

        val notification = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle(label)
            .setContentText("Alarm")
            .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
            .setCategory(Notification.CATEGORY_ALARM)
            .setOngoing(true)
            .setFullScreenIntent(fullScreen, true)
            .addAction(
                Notification.Action.Builder(null, "Dismiss", dismiss).build(),
            )
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    companion object {
        private const val TAG = "ClipperAlarm"
        private const val CHANNEL_ID = "clipper.alarm.ringing"
        private const val NOTIFICATION_ID = 4711
        private const val DEFAULT_LABEL = "Alarm"

        /**
         * A ringing alarm nobody answers should not hold the CPU awake forever.
         * Ten minutes is well past any alarm a person is going to respond to.
         */
        private const val WAKE_LOCK_TIMEOUT_MS = 10 * 60 * 1000L

        fun start(context: Context, label: String, itemId: String, occurrenceKey: String) {
            val intent = Intent(context, RingService::class.java).apply {
                putExtra(AlarmIntents.EXTRA_LABEL, label)
                putExtra(AlarmIntents.EXTRA_ITEM_ID, itemId)
                putExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY, occurrenceKey)
            }
            runCatching { context.startForegroundService(intent) }
                .onFailure { Log.e(TAG, "Could not start the ring service", it) }
        }

        fun dismiss(context: Context) {
            val intent = Intent(context, RingService::class.java)
                .setAction(AlarmIntents.ACTION_DISMISS)
            runCatching { context.startService(intent) }
        }

        /**
         * The channel must exist before the notification, and it is created on
         * the device-protected context so it survives being needed before
         * unlock.
         */
        private fun ensureChannel(context: Context) {
            val manager = context.getSystemService(NotificationManager::class.java) ?: return
            if (manager.getNotificationChannel(CHANNEL_ID) != null) return
            val channel = NotificationChannel(
                CHANNEL_ID,
                "Ringing alarms",
                NotificationManager.IMPORTANCE_HIGH,
            ).apply {
                description = "Shown while an alarm is ringing"
                setBypassDnd(true)
                lockscreenVisibility = Notification.VISIBILITY_PUBLIC
                // The service owns the sound so it can loop and be stopped; a
                // channel sound would play once and ignore the alarm stream.
                setSound(null, null)
                enableVibration(false)
            }
            manager.createNotificationChannel(channel)
        }
    }
}
