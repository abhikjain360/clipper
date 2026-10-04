package com.clipper.alarm

import android.app.AlarmManager
import android.app.Application
import android.app.NotificationManager
import android.content.Intent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowAlarmManager

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28], application = Application::class)
class RestTimerTest {
    private lateinit var context: Application
    private lateinit var alarms: AlarmManager
    private lateinit var notifications: NotificationManager

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        alarms = context.getSystemService(AlarmManager::class.java)
        notifications = context.getSystemService(NotificationManager::class.java)
        ShadowAlarmManager.setAutoSchedule(false)
        RestTimer.cancel(context, null)
    }

    @Test
    fun theLatestRestReplacesTheEarlierAlarmClock() {
        val now = System.currentTimeMillis()
        RestTimer.schedule(context, "session", now + 90_000L, "Rest is over", "Next: Squat")
        RestTimer.schedule(context, "session", now + 180_000L, "Rest is over", "Next: Squat")

        val scheduled = shadowOf(alarms).scheduledAlarms.single()
        assertEquals(now + 180_000L, scheduled.triggerAtMs)
        assertNotNull(scheduled.alarmClockInfo)
    }

    @Test
    fun onlyTheArmedRestOfTheArmedSessionNotifies() {
        val now = System.currentTimeMillis()
        RestTimer.schedule(context, "first", now + 60_000L, "Rest is over", "Next: Squat")
        val earlierRest = Intent(armedDelivery())
        RestTimer.schedule(context, "first", now + 120_000L, "Rest is over", "Next: Squat")
        RestTimerReceiver().onReceive(context, earlierRest)
        assertTrue(shadowOf(notifications).allNotifications.isEmpty())

        val armed = Intent(armedDelivery())
        RestTimer.cancel(context, "second")
        assertEquals(1, shadowOf(alarms).scheduledAlarms.size)
        RestTimer.cancel(context, "first")
        assertTrue(shadowOf(alarms).scheduledAlarms.isEmpty())
        RestTimerReceiver().onReceive(context, armed)
        assertTrue(shadowOf(notifications).allNotifications.isEmpty())

        RestTimer.schedule(context, "first", now + 120_000L, "Rest is over", "Next: Squat")
        RestTimerReceiver().onReceive(context, armedDelivery())
        assertEquals(1, shadowOf(notifications).allNotifications.size)
    }

    private fun armedDelivery(): Intent =
        shadowOf(shadowOf(alarms).scheduledAlarms.single().operation).savedIntent
}
