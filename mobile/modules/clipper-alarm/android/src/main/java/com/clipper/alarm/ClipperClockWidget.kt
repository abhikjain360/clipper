package com.clipper.alarm

import android.app.AlarmManager
import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.util.Log
import android.view.View
import android.widget.RemoteViews
import java.util.TimeZone

class ClipperClockWidget : AppWidgetProvider() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == ACTION_REFRESH) {
            updateAll(context)
            return
        }
        super.onReceive(context, intent)
    }

    override fun onUpdate(context: Context, manager: AppWidgetManager, appWidgetIds: IntArray) {
        updateAll(context)
    }

    override fun onEnabled(context: Context) {
        updateAll(context)
    }

    override fun onDeleted(context: Context, appWidgetIds: IntArray) {
        appWidgetIds.forEach { ClipperClockWidgetPrefs.delete(context, it) }
        updateAll(context)
    }

    override fun onDisabled(context: Context) {
        cancelDateRefresh(context)
    }

    companion object {
        private const val ACTION_REFRESH = "com.clipper.alarm.action.WIDGET_REFRESH"
        private const val REQUEST_REFRESH = 0x7100
        private const val REQUEST_OPEN_APP = 0x7101

        fun updateAll(context: Context) {
            runCatching {
                val appContext = context.applicationContext
                val manager = AppWidgetManager.getInstance(appContext)
                val ids = manager.getAppWidgetIds(ComponentName(appContext, ClipperClockWidget::class.java))
                if (ids.isEmpty()) {
                    cancelDateRefresh(appContext)
                    return
                }
                val now = System.currentTimeMillis()
                val nextAlarm = ClipperClockText.nextAlarm(AlarmMirror.loadUpcoming(appContext), now)
                val zones = mutableSetOf(TimeZone.getDefault().id)
                ids.forEach { id ->
                    val config = ClipperClockWidgetPrefs.load(appContext, id)
                    listOfNotNull(config.zoneOneId, config.zoneTwoId).forEach { zones.add(it) }
                    manager.updateAppWidget(id, buildViews(appContext, config, nextAlarm, now))
                }
                scheduleDateRefresh(appContext, zones, now)
            }.onFailure { Log.w("ClipperAlarm", "Could not update clock widgets", it) }
        }

        private fun buildViews(
            context: Context,
            config: ClipperClockWidgetConfig,
            nextAlarm: String,
            now: Long,
        ): RemoteViews {
            val views = RemoteViews(context.packageName, R.layout.widget_clipper_clock)
            openAppIntent(context)?.let { views.setOnClickPendingIntent(R.id.widget_root, it) }
            views.setTextViewText(R.id.widget_next_alarm, nextAlarm)
            bindZone(views, R.id.widget_zone_one, R.id.widget_zone_one_clock,
                R.id.widget_zone_one_label, config.zoneOneId, config.zoneOneLabel, now)
            bindZone(views, R.id.widget_zone_two, R.id.widget_zone_two_clock,
                R.id.widget_zone_two_label, config.zoneTwoId, config.zoneTwoLabel, now)
            views.setViewVisibility(R.id.widget_zone_gap,
                if (config.zoneOneId != null && config.zoneTwoId != null) View.VISIBLE else View.GONE)
            return views
        }

        private fun bindZone(
            views: RemoteViews,
            containerId: Int,
            clockId: Int,
            labelId: Int,
            zoneId: String?,
            label: String,
            now: Long,
        ) {
            views.setViewVisibility(containerId, if (zoneId == null) View.GONE else View.VISIBLE)
            if (zoneId == null) return
            views.setString(clockId, "setTimeZone", zoneId)
            views.setTextViewText(labelId, ClipperClockText.secondaryZoneLabel(zoneId, label, now))
        }

        private fun openAppIntent(context: Context): PendingIntent? {
            val intent = context.packageManager.getLaunchIntentForPackage(context.packageName)
                ?: return null
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            return PendingIntent.getActivity(context, REQUEST_OPEN_APP, intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        }

        private fun scheduleDateRefresh(context: Context, zones: Set<String>, now: Long) {
            val next = zones.minOf { ClipperClockText.nextMidnight(now, TimeZone.getTimeZone(it)) }
            val pending = PendingIntent.getBroadcast(context, REQUEST_REFRESH, refreshIntent(context),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
            context.getSystemService(AlarmManager::class.java).set(AlarmManager.RTC, next, pending)
        }

        private fun cancelDateRefresh(context: Context) {
            val pending = PendingIntent.getBroadcast(context, REQUEST_REFRESH, refreshIntent(context),
                PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE) ?: return
            context.getSystemService(AlarmManager::class.java).cancel(pending)
            pending.cancel()
        }

        private fun refreshIntent(context: Context): Intent =
            Intent(context, ClipperClockWidget::class.java).setAction(ACTION_REFRESH)
    }
}
