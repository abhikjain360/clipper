package com.clipper.alarm

import java.text.SimpleDateFormat
import java.util.Calendar
import java.util.Date
import java.util.Locale
import java.util.TimeZone

internal object ClipperClockText {
    fun nextAlarm(
        plan: List<PlannedAlarm>,
        now: Long = System.currentTimeMillis(),
        zone: TimeZone = TimeZone.getDefault(),
    ): String {
        val next = plan.filter { it.fireAtMillis > now }.minByOrNull { it.fireAtMillis }
            ?: return "No alarm"
        return formatNextAlarm(next.fireAtMillis, now, zone)
    }

    fun formatNextAlarm(next: Long, now: Long, zone: TimeZone): String {
        val today = day(now, zone)
        val nextDay = day(next, zone)
        val tomorrow = (today.clone() as Calendar).apply { add(Calendar.DATE, 1) }
        val week = (today.clone() as Calendar).apply { add(Calendar.DATE, 7) }
        return when {
            nextDay == today -> "Today ${format("HH:mm", next, zone)}"
            nextDay == tomorrow -> "Tomorrow ${format("HH:mm", next, zone)}"
            nextDay.before(week) -> format("EEE HH:mm", next, zone)
            else -> format("d MMM HH:mm", next, zone)
        }
    }

    fun secondaryZoneLabel(
        zoneId: String,
        label: String,
        now: Long = System.currentTimeMillis(),
        localZone: TimeZone = TimeZone.getDefault(),
    ): String {
        val zone = TimeZone.getTimeZone(zoneId)
        val base = label.ifBlank { defaultZoneLabel(zoneId) }
        val localDate = format("yyyy-MM-dd", now, localZone)
        val zoneDate = format("yyyy-MM-dd", now, zone)
        return if (zoneDate == localDate) base else "$base · ${format("EEE", now, zone)}"
    }

    fun nextMidnight(now: Long, zone: TimeZone): Long =
        day(now, zone).apply { add(Calendar.DATE, 1) }.timeInMillis + 2_000L

    private fun day(millis: Long, zone: TimeZone): Calendar =
        Calendar.getInstance(zone).apply {
            timeInMillis = millis
            set(Calendar.HOUR_OF_DAY, 0)
            set(Calendar.MINUTE, 0)
            set(Calendar.SECOND, 0)
            set(Calendar.MILLISECOND, 0)
        }

    private fun format(pattern: String, millis: Long, zone: TimeZone): String =
        SimpleDateFormat(pattern, Locale.getDefault()).apply { timeZone = zone }.format(Date(millis))
}

internal fun defaultZoneLabel(zoneId: String): String =
    if (zoneId == "Asia/Kolkata") "Delhi" else zoneId.substringAfterLast('/').replace('_', ' ')
