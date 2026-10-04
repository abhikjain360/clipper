package com.clipper.alarm

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import java.time.Instant
import java.util.Locale
import java.util.TimeZone

class ClipperClockWidgetTest {
    private val zone = TimeZone.getTimeZone("UTC")
    private lateinit var previousLocale: Locale

    @Before
    fun setLocale() {
        previousLocale = Locale.getDefault()
        Locale.setDefault(Locale.US)
    }

    @After
    fun restoreLocale() {
        Locale.setDefault(previousLocale)
    }

    @Test
    fun formatsTodayAndTomorrowNextAlarmText() {
        val now = millis("2026-06-12T18:54:00Z")
        assertEquals("Today 19:00", ClipperClockText.formatNextAlarm(
            millis("2026-06-12T19:00:00Z"), now, zone))
        assertEquals("Tomorrow 07:30", ClipperClockText.formatNextAlarm(
            millis("2026-06-13T07:30:00Z"), now, zone))
    }

    @Test
    fun formatsWeekdayAndDateFallbacks() {
        val now = millis("2026-06-12T18:54:00Z")
        assertEquals("Sun 07:30", ClipperClockText.formatNextAlarm(
            millis("2026-06-14T07:30:00Z"), now, zone))
        assertEquals("Thu 07:30", ClipperClockText.formatNextAlarm(
            millis("2026-06-18T07:30:00Z"), now, zone))
        assertEquals("19 Jun 07:30", ClipperClockText.formatNextAlarm(
            millis("2026-06-19T07:30:00Z"), now, zone))
    }

    @Test
    fun picksEarliestFutureAlarmFromUnsortedPlan() {
        val now = millis("2026-06-12T18:54:00Z")
        val plan = listOf(alarm("2026-06-13T07:30:00Z"), alarm("2026-06-12T18:54:00Z"),
            alarm("2026-06-12T19:00:00Z"), alarm("2026-06-12T18:00:00Z"))
        assertEquals("Today 19:00", ClipperClockText.nextAlarm(plan, now, zone))
    }

    @Test
    fun showsNoAlarmForEmptyOrExpiredPlan() {
        val now = millis("2026-06-12T18:54:00Z")
        assertEquals("No alarm", ClipperClockText.nextAlarm(emptyList(), now, zone))
        assertEquals("No alarm", ClipperClockText.nextAlarm(
            listOf(alarm("2026-06-12T18:54:00Z"), alarm("2026-06-12T18:00:00Z")), now, zone))
    }

    @Test
    fun secondaryZoneLabelAddsDayOnlyWhenDateDiffers() {
        assertEquals("Tokyo · Sat", ClipperClockText.secondaryZoneLabel(
            "Asia/Tokyo", "Tokyo", millis("2026-06-12T23:30:00Z"), zone))
        assertEquals("London", ClipperClockText.secondaryZoneLabel(
            "Europe/London", "", millis("2026-06-12T12:00:00Z"), zone))
        assertEquals("Office · Thu", ClipperClockText.secondaryZoneLabel(
            "America/Los_Angeles", "Office", millis("2026-06-12T01:30:00Z"), zone))
    }

    @Test
    fun usesLocalDateAcrossDaylightSavingChange() {
        val berlin = TimeZone.getTimeZone("Europe/Berlin")
        val now = millis("2026-03-28T23:30:00Z")
        assertEquals("Today 03:30", ClipperClockText.formatNextAlarm(
            millis("2026-03-29T01:30:00Z"), now, berlin))
        assertEquals("Tomorrow 07:30", ClipperClockText.formatNextAlarm(
            millis("2026-03-30T05:30:00Z"), now, berlin))
        assertEquals(millis("2026-03-29T22:00:02Z"), ClipperClockText.nextMidnight(now, berlin))
    }

    private fun alarm(instant: String): PlannedAlarm = PlannedAlarm(
        "item", instant, "Alarm", millis(instant), millis(instant))

    private fun millis(instant: String): Long = Instant.parse(instant).toEpochMilli()
}
