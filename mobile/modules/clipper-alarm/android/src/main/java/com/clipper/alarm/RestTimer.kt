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
        RestTimer.show(
            context.applicationContext,
            intent.getStringExtra(RestTimer.EXTRA_TITLE) ?: "Rest is over",
            intent.getStringExtra(RestTimer.EXTRA_TEXT).orEmpty(),
        )
    }
}

object RestTimer {
    const val ACTION_REST_OVER = "com.clipper.alarm.action.REST_OVER"
    const val EXTRA_TITLE = "com.clipper.alarm.extra.REST_TITLE"
    const val EXTRA_TEXT = "com.clipper.alarm.extra.REST_TEXT"
    private const val CHANNEL_ID = "clipper.gym.rest"
    private const val NOTIFICATION_ID = 4712
    private const val REQUEST_CODE = 4712

    @SuppressLint("MissingPermission")
    fun schedule(context: Context, atMillis: Long, title: String, text: String) {
        val alarmManager = context.getSystemService(AlarmManager::class.java)
        context.getSystemService(NotificationManager::class.java)?.cancel(NOTIFICATION_ID)
        val intent = Intent(context, RestTimerReceiver::class.java).apply {
            action = ACTION_REST_OVER
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
            alarmManager.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, atMillis, pending)
        } else {
            alarmManager.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, atMillis, pending)
        }
    }

    fun cancel(context: Context) {
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
        context.getSystemService(NotificationManager::class.java)?.cancel(NOTIFICATION_ID)
    }

    fun show(context: Context, title: String, text: String) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
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
        val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
            ?: Intent()
        val open = PendingIntent.getActivity(context, REQUEST_CODE, launch, PendingIntent.FLAG_IMMUTABLE)
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
            .setContentIntent(open)
            .build()
        runCatching { manager.notify(NOTIFICATION_ID, notification) }
    }
}
