package com.clipper.alarm

import android.annotation.SuppressLint
import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build

class RestTimerReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != RestTimer.ACTION_REST_OVER) return
        RestTimer.deliver(context.applicationContext, intent)
    }
}

object RestTimer {
    const val ACTION_REST_OVER = "com.clipper.alarm.action.REST_OVER"
    private const val EXTRA_SESSION_ID = "com.clipper.alarm.extra.REST_SESSION_ID"
    private const val EXTRA_ENDS_AT = "com.clipper.alarm.extra.REST_ENDS_AT"
    private const val EXTRA_TITLE = "com.clipper.alarm.extra.REST_TITLE"
    private const val EXTRA_TEXT = "com.clipper.alarm.extra.REST_TEXT"
    private const val PREFERENCES = "clipper.gym.rest"
    private const val ARMED_SESSION_ID = "session_id"
    private const val ARMED_ENDS_AT = "ends_at"
    private const val CHANNEL_ID = "clipper.gym.rest"
    private const val NOTIFICATION_ID = 4712
    private const val REQUEST_CODE = 4712

    @SuppressLint("MissingPermission")
    fun schedule(context: Context, sessionId: String, endsAtMillis: Long, title: String, text: String) {
        val alarmManager = context.getSystemService(AlarmManager::class.java)
        notificationManager(context)?.cancel(NOTIFICATION_ID)
        preferences(context).edit()
            .putString(ARMED_SESSION_ID, sessionId)
            .putLong(ARMED_ENDS_AT, endsAtMillis)
            .commit()
        val intent = Intent(context, RestTimerReceiver::class.java).apply {
            action = ACTION_REST_OVER
            putExtra(EXTRA_SESSION_ID, sessionId)
            putExtra(EXTRA_ENDS_AT, endsAtMillis)
            putExtra(EXTRA_TITLE, title)
            putExtra(EXTRA_TEXT, text)
        }
        val pending = PendingIntent.getBroadcast(
            context,
            REQUEST_CODE,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val exact = Build.VERSION.SDK_INT < Build.VERSION_CODES.S ||
            alarmManager.canScheduleExactAlarms()
        if (exact) {
            alarmManager.setAlarmClock(
                AlarmManager.AlarmClockInfo(endsAtMillis, openApp(context)),
                pending,
            )
        } else {
            alarmManager.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, endsAtMillis, pending)
        }
    }

    fun cancel(context: Context, sessionId: String?) {
        val armed = preferences(context).getString(ARMED_SESSION_ID, null)
        if (sessionId != null && armed != null && armed != sessionId) return
        preferences(context).edit().clear().commit()
        val intent = Intent(context, RestTimerReceiver::class.java).apply {
            action = ACTION_REST_OVER
        }
        PendingIntent.getBroadcast(
            context,
            REQUEST_CODE,
            intent,
            PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE,
        )?.let { pending ->
            context.getSystemService(AlarmManager::class.java).cancel(pending)
            pending.cancel()
        }
        notificationManager(context)?.cancel(NOTIFICATION_ID)
    }

    fun deliver(context: Context, intent: Intent) {
        val preferences = preferences(context)
        val armedSession = preferences.getString(ARMED_SESSION_ID, null)
        val armedEnd = preferences.getLong(ARMED_ENDS_AT, -1L)
        val sessionId = intent.getStringExtra(EXTRA_SESSION_ID)
        val endsAt = intent.getLongExtra(EXTRA_ENDS_AT, -2L)
        if (armedSession == null || armedSession != sessionId || armedEnd != endsAt) return
        preferences.edit().clear().commit()
        show(
            context,
            intent.getStringExtra(EXTRA_TITLE) ?: "Rest is over",
            intent.getStringExtra(EXTRA_TEXT).orEmpty(),
        )
    }

    private fun show(context: Context, title: String, text: String) {
        val manager = notificationManager(context) ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
            manager.getNotificationChannel(CHANNEL_ID) == null
        ) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "Gym rest timer", NotificationManager.IMPORTANCE_HIGH)
                    .apply {
                        description = "Tells you when the rest between sets is over"
                        enableVibration(true)
                        vibrationPattern = longArrayOf(0, 400, 200, 400)
                    },
            )
        }
        val notification = (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(context, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(context)
                .setPriority(Notification.PRIORITY_HIGH)
                .setDefaults(Notification.DEFAULT_ALL)
        })
            .setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
            .setCategory(Notification.CATEGORY_REMINDER)
            .setAutoCancel(true)
            .setContentIntent(openApp(context))
            .build()
        runCatching { manager.notify(NOTIFICATION_ID, notification) }
    }

    private fun openApp(context: Context): PendingIntent {
        val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
            ?: Intent()
        return PendingIntent.getActivity(context, REQUEST_CODE, launch, PendingIntent.FLAG_IMMUTABLE)
    }

    private fun preferences(context: Context) =
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

    private fun notificationManager(context: Context) =
        context.getSystemService(NotificationManager::class.java)
}
