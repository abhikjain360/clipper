package com.clipper.alarm

import android.app.NotificationManager
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import org.json.JSONArray

/**
 * The JS surface of the alarm layer.
 *
 * Small on purpose. Rust decides when an alarm rings and sends instants down.
 * This module registers them, reports whether the OS will honour exact alarms,
 * and can send the user to the setting when it will not.
 *
 * The plan crosses as a JSON string rather than a list of typed records.
 * Expo's argument marshalling relies on reified generics that a `List<Record>`
 * parameter does not survive here, and it fails at runtime rather than at
 * compile time. The plan is already JSON on both sides, so a string costs
 * nothing.
 */
class ClipperAlarmModule : Module() {

    override fun definition() = ModuleDefinition {
        Name("ClipperAlarm")

        /**
         * Replace the entire registered set with the plan in [planJson].
         *
         * Returns how many were armed, at most `AlarmScheduler.MAX_REGISTERED`.
         * Each fire rolls the horizon forward from the device-protected
         * mirror.
         */
        Function("setAlarms") { planJson: String ->
            AlarmScheduler(context).replaceAll(parsePlan(planJson))
        }

        Function("cancelAll") {
            AlarmScheduler(context).cancelAll()
            AlarmMirror.clear(context)
        }

        /**
         * Whether the OS will deliver exact alarms. False means every alarm
         * is left unarmed, so the UI should say so rather than pretend.
         */
        Function("canScheduleExactAlarms") {
            AlarmScheduler(context).canScheduleExactAlarms()
        }

        Function("areNotificationsEnabled") {
            notificationManager.areNotificationsEnabled() && (
                Build.VERSION.SDK_INT < Build.VERSION_CODES.O ||
                    notificationManager.getNotificationChannel(RingService.CHANNEL_ID)
                        ?.importance != NotificationManager.IMPORTANCE_NONE
                )
        }

        Function("canUseFullScreenIntent") {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                notificationManager.canUseFullScreenIntent()
            } else {
                true
            }
        }

        /** Open the system screen where exact alarms are granted. */
        Function("openExactAlarmSettings") {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
                return@Function false
            }
            val intent = Intent(Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM).apply {
                data = Uri.fromParts("package", context.packageName, null)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            runCatching { context.startActivity(intent) }.isSuccess
        }

        Function("openNotificationSettings") {
            val intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                val channelBlocked = notificationManager
                    .getNotificationChannel(RingService.CHANNEL_ID)
                    ?.importance == NotificationManager.IMPORTANCE_NONE
                Intent(
                    if (channelBlocked) Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS
                    else Settings.ACTION_APP_NOTIFICATION_SETTINGS,
                ).apply {
                    putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
                    if (channelBlocked) putExtra(Settings.EXTRA_CHANNEL_ID, RingService.CHANNEL_ID)
                }
            } else {
                Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
                    data = Uri.fromParts("package", context.packageName, null)
                }
            }
            intent.apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            runCatching { context.startActivity(intent) }.isSuccess
        }

        Function("openFullScreenIntentSettings") {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                return@Function false
            }
            val intent = Intent(Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT).apply {
                data = Uri.fromParts("package", context.packageName, null)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            runCatching { context.startActivity(intent) }.isSuccess
        }

        /** What the device-protected mirror currently holds, for diagnostics. */
        Function("plannedCount") {
            val now = System.currentTimeMillis()
            AlarmMirror.load(context).count { alarm -> alarm.fireAtMillis > now }
        }

        /**
         * Ring immediately. Exercises the whole alarm path — permissions,
         * foreground service, full-screen intent — without waiting for a real
         * alarm time.
         */
        Function("ringNow") { label: String ->
            RingService.start(context, label, "", "")
        }

        Function("dismiss") {
            RingService.dismiss(context)
        }
    }

    /**
     * A malformed entry is skipped rather than failing the whole plan: one bad
     * alarm should not silence the rest.
     */
    private fun parsePlan(planJson: String): List<PlannedAlarm> = runCatching {
        val array = JSONArray(planJson)
        (0 until array.length()).mapNotNull { index ->
            array.optJSONObject(index)?.let(PlannedAlarm::fromJson)
        }
    }.getOrDefault(emptyList())

    private val context
        get() = requireNotNull(appContext.reactContext) {
            "ClipperAlarm needs a React context"
        }

    private val notificationManager
        get() = requireNotNull(context.getSystemService(NotificationManager::class.java)) {
            "NotificationManager is unavailable"
        }
}
