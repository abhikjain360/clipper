package com.clipper.alarm

import android.app.ActivityManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.os.Process
import android.util.Log
import android.widget.Toast

/**
 * Holds ringing alarms alive.
 *
 * A foreground service rather than an activity alone, because an activity can
 * be swiped away or never shown, while the sound has to keep going until all
 * alarms are handled or the ten-minute auto-silence window expires. The `mediaPlayback`
 * type permits audio from the background. Its notification carries a
 * full-screen intent, which is how the ring screen appears over the lock
 * screen.
 *
 * A wake lock guards the gap between the alarm firing and the screen coming on.
 * Without it the device can return to sleep mid-start on some vendors.
 */
class RingService : Service() {

    private val handler = Handler(Looper.getMainLooper())
    private val autoSilence = Runnable {
        Log.i(TAG, "Unanswered alarm silenced after ten minutes")
        stopRinging()
    }
    private var ringer: Ringer? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private data class RingingAlarm(val alarm: PlannedAlarm, val generation: Long)

    private val ringingAlarms = mutableListOf<RingingAlarm>()

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // START_STICKY may ask the system to recreate a killed service without
        // the original extras. Never turn that lifecycle callback into a new,
        // unlabeled alarm; only an explicit alarm delivery may start ringing.
        if (intent == null) {
            Log.w(TAG, "Ignoring sticky restart without an alarm intent")
            stopSelf(startId)
            return START_NOT_STICKY
        }

        when (intent.action) {
            AlarmIntents.ACTION_SNOOZE -> {
                val ringing = ringingAlarms.firstOrNull { matches(intent, it.alarm) }
                    ?: return ringingResult(startId)
                val alarm = ringing.alarm
                if (!alarm.canSnooze) {
                    ringingAlarms.remove(ringing)
                    updateRinging()
                    return ringingResult(startId)
                }
                return try {
                    AlarmScheduler(this).snooze(alarm, ringing.generation)
                    ringingAlarms.remove(ringing)
                    updateRinging()
                    ringingResult(startId)
                } catch (error: Throwable) {
                    Log.e(TAG, "Could not snooze the alarm", error)
                    Toast.makeText(this, "Could not snooze the alarm", Toast.LENGTH_LONG).show()
                    START_STICKY
                }
            }
            AlarmIntents.ACTION_DISMISS -> {
                if (!hasIdentity(intent)) {
                    stopRinging()
                } else if (ringingAlarms.removeAll { matches(intent, it.alarm) }) {
                    updateRinging()
                }
                return ringingResult(startId)
            }
            AlarmIntents.ACTION_PLAN_CHANGED -> {
                val plan = AlarmMirror.load(this)
                if (ringingAlarms.removeAll { ringing ->
                    !ringing.alarm.canSnooze && plan.none { sameAlarm(it, ringing.alarm) }
                }) {
                    updateRinging()
                }
                return ringingResult(startId)
            }
        }

        val generation = intent.getLongExtra(AlarmIntents.EXTRA_GENERATION, -1L)
        if (generation != AlarmMirror.generation(this)) {
            Log.i(TAG, "Ignoring cancelled ring request")
            return ringingResult(startId)
        }

        val label = intent.getStringExtra(AlarmIntents.EXTRA_LABEL) ?: DEFAULT_LABEL
        val itemId = intent.getStringExtra(AlarmIntents.EXTRA_ITEM_ID).orEmpty()
        val occurrenceKey = intent.getStringExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY).orEmpty()
        val fireAt = intent.getLongExtra(AlarmIntents.EXTRA_FIRE_AT, System.currentTimeMillis())
        val canSnooze = intent.getBooleanExtra(AlarmIntents.EXTRA_CAN_SNOOZE, true)
        val alarm = PlannedAlarm(itemId, occurrenceKey, label, fireAt,
            intent.getLongExtra(AlarmIntents.EXTRA_START, fireAt), canSnooze)
        if (ringingAlarms.none { sameAlarm(it.alarm, alarm) }) {
            ringingAlarms.add(RingingAlarm(alarm, generation))
        }
        // Android 10+ restricts background activity launches. The full-screen
        // intent on the alarm notification is the supported path while the
        // app is backgrounded or the device is locked; a direct launch is
        // useful only when the user is already looking at Clipper.
        val appWasVisible = isAppVisible()

        acquireWakeLock()
        updateRinging()
        handler.removeCallbacks(autoSilence)
        handler.postDelayed(autoSilence, AUTO_SILENCE_MS)

        if (ringer == null) {
            ringer = Ringer(this).also { it.start(vibrate = true) }
        }

        if (appWasVisible) {
            runCatching {
                startActivity(
                    RingActivity.intent(this, requireNotNull(displayedAlarm))
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                )
            }.onFailure { Log.w(TAG, "Could not show ring activity", it) }
        }

        // Keep the service alive while the alarm is ringing. A null sticky
        // restart is handled above so it cannot create a phantom ring.
        return START_STICKY
    }

    override fun onDestroy() {
        stopRinging()
        super.onDestroy()
    }

    private fun ringingResult(startId: Int): Int {
        if (ringingAlarms.isNotEmpty()) return START_STICKY
        stopSelf(startId)
        return START_NOT_STICKY
    }

    private fun updateRinging() {
        val alarm = (ringingAlarms.firstOrNull { it.alarm.canSnooze }
            ?: ringingAlarms.firstOrNull())?.alarm
        if (alarm == null) {
            stopRinging()
            return
        }
        val changed = displayedAlarm != alarm
        displayedAlarm = alarm
        isRinging = true
        startForegroundWithNotification(alarm)
        ClipperClockWidget.updateAll(this)
        if (changed) alarmListeners.toList().forEach { it(alarm) }
    }

    private fun stopRinging() {
        handler.removeCallbacks(autoSilence)
        isRinging = false
        ringingAlarms.clear()
        displayedAlarm = null
        ClipperClockWidget.updateAll(this)
        stoppedListeners.toList().forEach { it() }
        ringer?.stop()
        ringer = null
        wakeLock?.let { if (it.isHeld) runCatching { it.release() } }
        wakeLock = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun acquireWakeLock() {
        if (wakeLock == null) {
            val power = getSystemService(PowerManager::class.java) ?: return
            wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "clipper:alarm")
                .apply { setReferenceCounted(false) }
        }
        wakeLock?.let { runCatching { it.acquire(WAKE_LOCK_TIMEOUT_MS) } }
    }

    private fun isAppVisible(): Boolean {
        val manager = getSystemService(ActivityManager::class.java) ?: return false
        val process = manager.runningAppProcesses
            ?.firstOrNull { it.pid == Process.myPid() }
        return process?.importance == ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND
    }

    private fun startForegroundWithNotification(alarm: PlannedAlarm) {
        // Notification channels and the channel-aware Notification.Builder
        // were added in API 26. The alarm module still supports API 24/25,
        // where the legacy builder is the only loadable path.
        ensureChannel(this)

        val fullScreen = PendingIntent.getActivity(
            this,
            0,
            RingActivity.intent(this, alarm),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

        val notification = (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        })
            .setContentTitle(alarm.label)
            .setContentText("Alarm")
            .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
            .setCategory(Notification.CATEGORY_ALARM)
            .setOngoing(true)
            .setContentIntent(fullScreen)
            .setFullScreenIntent(fullScreen, true)
            .apply {
                if (alarm.canSnooze) {
                    val snooze = PendingIntent.getService(this@RingService, 0,
                        actionIntent(this@RingService, AlarmIntents.ACTION_SNOOZE, alarm),
                        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
                    addAction(Notification.Action.Builder(null,
                        getString(R.string.clipper_alarm_snooze), snooze).build())
                }
                val dismiss = PendingIntent.getService(this@RingService, 0,
                    actionIntent(this@RingService, AlarmIntents.ACTION_DISMISS, alarm),
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
                addAction(Notification.Action.Builder(null,
                    getString(R.string.clipper_alarm_dismiss), dismiss).build())
            }
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
        const val CHANNEL_ID = "clipper.alarm.ringing"
        private const val NOTIFICATION_ID = 4711
        private const val DEFAULT_LABEL = "Alarm"

        // Keep the CPU awake through the auto-silence callback, with a bounded
        // safety timeout if cleanup fails.
        private const val AUTO_SILENCE_MS = 10 * 60 * 1000L
        private const val WAKE_LOCK_TIMEOUT_MS = AUTO_SILENCE_MS + 30_000L

        // Service and activity lifecycle callbacks run on the main thread.
        @Volatile
        internal var isRinging = false
            private set
        internal var displayedAlarm: PlannedAlarm? = null
            private set
        internal val stoppedListeners = mutableSetOf<() -> Unit>()
        internal val alarmListeners = mutableSetOf<(PlannedAlarm) -> Unit>()

        fun start(context: Context, label: String, itemId: String, occurrenceKey: String) {
            val now = System.currentTimeMillis()
            start(context, PlannedAlarm(itemId, occurrenceKey, label, now, now),
                AlarmMirror.generation(context))
        }

        internal fun start(context: Context, alarm: PlannedAlarm, generation: Long) {
            val intent = Intent(context, RingService::class.java).apply {
                putExtra(AlarmIntents.EXTRA_LABEL, alarm.label)
                putExtra(AlarmIntents.EXTRA_ITEM_ID, alarm.itemId)
                putExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY, alarm.occurrenceKey)
                putExtra(AlarmIntents.EXTRA_FIRE_AT, alarm.fireAtMillis)
                putExtra(AlarmIntents.EXTRA_START, alarm.occurrenceStartMillis)
                putExtra(AlarmIntents.EXTRA_CAN_SNOOZE, alarm.canSnooze)
                putExtra(AlarmIntents.EXTRA_GENERATION, generation)
            }
            runCatching {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    context.startForegroundService(intent)
                } else {
                    context.startService(intent)
                }
            }
                .onFailure { Log.e(TAG, "Could not start the ring service", it) }
        }

        fun dismiss(context: Context, alarm: PlannedAlarm? = null) {
            val intent = actionIntent(context, AlarmIntents.ACTION_DISMISS, alarm)
            runCatching { context.startService(intent) }
        }

        fun snooze(context: Context, alarm: PlannedAlarm? = displayedAlarm) {
            if (alarm == null) return
            val intent = actionIntent(context, AlarmIntents.ACTION_SNOOZE, alarm)
            runCatching { context.startService(intent) }
                .onFailure { Log.e(TAG, "Could not snooze the alarm", it) }
        }

        internal fun planChanged(context: Context) {
            if (!isRinging) return
            val intent = Intent(context, RingService::class.java)
                .setAction(AlarmIntents.ACTION_PLAN_CHANGED)
            runCatching { context.startService(intent) }
                .onFailure { Log.e(TAG, "Could not update ringing timers", it) }
        }

        private fun actionIntent(context: Context, action: String, alarm: PlannedAlarm?): Intent =
            Intent(context, RingService::class.java).setAction(action).apply {
                if (alarm != null) {
                    data = Uri.Builder().scheme("clipper-alarm").authority("ring")
                        .appendPath(alarm.itemId).appendPath(alarm.occurrenceKey)
                        .appendPath(alarm.fireAtMillis.toString()).build()
                    putExtra(AlarmIntents.EXTRA_ITEM_ID, alarm.itemId)
                    putExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY, alarm.occurrenceKey)
                    putExtra(AlarmIntents.EXTRA_FIRE_AT, alarm.fireAtMillis)
                }
            }

        private fun hasIdentity(intent: Intent): Boolean =
            intent.hasExtra(AlarmIntents.EXTRA_ITEM_ID) ||
                intent.hasExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY) ||
                intent.hasExtra(AlarmIntents.EXTRA_FIRE_AT)

        private fun matches(intent: Intent, alarm: PlannedAlarm): Boolean =
            intent.getStringExtra(AlarmIntents.EXTRA_ITEM_ID) == alarm.itemId &&
                intent.getStringExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY) == alarm.occurrenceKey &&
                intent.hasExtra(AlarmIntents.EXTRA_FIRE_AT) &&
                intent.getLongExtra(AlarmIntents.EXTRA_FIRE_AT, -1L) == alarm.fireAtMillis

        private fun sameAlarm(first: PlannedAlarm, second: PlannedAlarm): Boolean =
            first.itemId == second.itemId && first.occurrenceKey == second.occurrenceKey &&
                first.fireAtMillis == second.fireAtMillis

        /**
         * The channel must exist before the notification, and it is created on
         * the device-protected context so it survives being needed before
         * unlock.
         */
        private fun ensureChannel(context: Context) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
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
