package com.clipper.alarm

import android.app.AlarmManager
import android.app.Application
import android.content.Intent
import android.os.Looper
import android.os.SystemClock
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.android.controller.ServiceController
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowAlarmManager
import java.util.TimeZone

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28], application = Application::class)
class SnoozeTest {
    private lateinit var context: Application
    private lateinit var scheduler: AlarmScheduler
    private lateinit var manager: AlarmManager
    private var service: ServiceController<RingService>? = null

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        manager = context.getSystemService(AlarmManager::class.java)
        scheduler = AlarmScheduler(context)
        ShadowAlarmManager.setAutoSchedule(false)
        AlarmMirror.clear(context)
    }

    @After
    fun tearDown() {
        service?.destroy()
        AlarmMirror.clear(context)
    }

    @Test
    fun snoozeStopsRingingAndSchedulesAnExactAlarmTenMinutesLater() {
        startRinging(alarm())
        val ringing = requireNotNull(service).get()
        val now = System.currentTimeMillis()
        RingService.snooze(context)
        ringing.onStartCommand(shadowOf(context).nextStartedService, 0, 2)

        assertFalse(RingService.isRinging)
        assertTrue(shadowOf(ringing).isForegroundStopped)
        val scheduled = shadowOf(manager).scheduledAlarms.single()
        assertTrue(scheduled.triggerAtMs in
            (now + AlarmScheduler.SNOOZE_MS)..(System.currentTimeMillis() + AlarmScheduler.SNOOZE_MS))
        assertNotNull(scheduled.alarmClockInfo)
        assertEquals(0L, scheduled.windowLengthMs)
        assertEquals(AlarmManager.RTC_WAKEUP, scheduled.getType())
    }

    @Test
    fun ringScreenSlideLeftSnoozes() {
        val alarm = alarm()
        startRinging(alarm)
        val activity = Robolectric.buildActivity(RingActivity::class.java,
            RingActivity.intent(context, alarm.label, alarm.itemId, alarm.occurrenceKey))
            .create().start().resume()
        try {
            val root = activity.get().findViewById<ViewGroup>(android.R.id.content)
            drag(requireNotNull(slider(root)), 0.05f)
            requireNotNull(service).get().onStartCommand(shadowOf(context).nextStartedService, 0, 2)
            assertFalse(RingService.isRinging)
            assertTrue(activity.get().isFinishing)
            assertEquals(1, AlarmMirror.loadSnoozes(context).size)
        } finally {
            activity.pause().stop().destroy()
        }
    }

    @Test
    fun ringScreenSlideRightDismisses() {
        val alarm = alarm()
        startRinging(alarm)
        val activity = Robolectric.buildActivity(RingActivity::class.java,
            RingActivity.intent(context, alarm.label, alarm.itemId, alarm.occurrenceKey))
            .create().start().resume()
        try {
            val root = activity.get().findViewById<ViewGroup>(android.R.id.content)
            drag(requireNotNull(slider(root)), 0.95f)
            requireNotNull(service).get().onStartCommand(shadowOf(context).nextStartedService, 0, 2)
            assertFalse(RingService.isRinging)
            assertTrue(activity.get().isFinishing)
            assertTrue(AlarmMirror.loadSnoozes(context).isEmpty())
        } finally {
            activity.pause().stop().destroy()
        }
    }

    @Test
    fun notificationTapOpensRingScreenWithoutStoppingTheAlarm() {
        val alarm = alarm()
        startRinging(alarm)
        val notification = shadowOf(requireNotNull(service).get()).lastForegroundNotification
        notification.contentIntent.send()
        val request = requireNotNull(shadowOf(context).nextStartedActivity)
        val activity = Robolectric.buildActivity(RingActivity::class.java, request)
            .create().start().resume()
        try {
            assertFalse(activity.get().isFinishing)
            assertTrue(RingService.isRinging)
            assertTrue(AlarmMirror.loadSnoozes(context).isEmpty())
            assertTrue(shadowOf(manager).scheduledAlarms.isEmpty())
        } finally {
            activity.pause().stop().destroy()
        }
    }

    @Test
    fun snoozeSurvivesPlanReplacementAndDoesNotReplaceAnotherSnooze() {
        val original = alarm()
        scheduler.replaceAll(listOf(original.copy(fireAtMillis = System.currentTimeMillis() + 3_600_000L)))
        scheduler.snooze(original, AlarmMirror.generation(context))
        scheduler.snooze(alarm("other"), AlarmMirror.generation(context))
        val snoozes = shadowOf(manager).scheduledAlarms.filter {
            shadowOf(it.operation).savedIntent.action == AlarmIntents.ACTION_SNOOZE_FIRE
        }.map { it.operation }.toSet()

        scheduler.replaceAll(emptyList())
        assertEquals(snoozes, shadowOf(manager).scheduledAlarms.map { it.operation }.toSet())
        scheduler.replaceAll(listOf(alarm("new").copy(fireAtMillis = System.currentTimeMillis() + 1_800_000L)))
        assertEquals(3, shadowOf(manager).scheduledAlarms.size)
        assertTrue(shadowOf(manager).scheduledAlarms.map { it.operation }.containsAll(snoozes))
        assertEquals(3, shadowOf(manager).scheduledAlarms.map {
            shadowOf(it.operation).requestCode
        }.toSet().size)
    }

    @Test
    fun bootRearmsSnoozesFromDeviceStorage() {
        scheduler.snooze(alarm(), AlarmMirror.generation(context))
        val expected = shadowOf(manager).scheduledAlarms.single().triggerAtMs
        shadowOf(manager).scheduledAlarms.forEach { manager.cancel(requireNotNull(it.operation)) }

        boot()

        val restored = shadowOf(manager).scheduledAlarms.single()
        assertEquals(expected, restored.triggerAtMs)
        assertNotNull(restored.alarmClockInfo)
        assertEquals(1, AlarmMirror.loadSnoozes(context.createDeviceProtectedStorageContext()).size)
    }

    @Test
    fun bootRearmsASnoozeThatBecameDueDuringReboot() {
        AlarmMirror.addSnooze(context, alarm().copy(fireAtMillis = System.currentTimeMillis() - 60_000L))
        val now = System.currentTimeMillis()

        boot()

        assertTrue(shadowOf(manager).scheduledAlarms.single().triggerAtMs in
            (now + 1_000L)..(System.currentTimeMillis() + 1_000L))
    }

    @Test
    fun repeatedSnoozesRingTheSameOccurrenceAndDismissalEndsIt() {
        val original = alarm()
        startRinging(original)
        repeat(3) {
            requireNotNull(service).get().onStartCommand(
                Intent(context, RingService::class.java).setAction(AlarmIntents.ACTION_SNOOZE), 0, 2)
            assertFalse(RingService.isRinging)
            val scheduled = shadowOf(manager).scheduledAlarms.single()
            manager.cancel(requireNotNull(scheduled.operation))
            AlarmReceiver().onReceive(context, shadowOf(scheduled.operation).savedIntent)
            assertTrue(AlarmMirror.loadSnoozes(context).isEmpty())
            val request = requireNotNull(shadowOf(context).nextStartedService)
            assertEquals(original.label, request.getStringExtra(AlarmIntents.EXTRA_LABEL))
            assertEquals(original.itemId, request.getStringExtra(AlarmIntents.EXTRA_ITEM_ID))
            assertEquals(original.occurrenceKey, request.getStringExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY))
            service?.destroy()
            service = Robolectric.buildService(RingService::class.java).create()
            requireNotNull(service).get().onStartCommand(request, 0, 1)
            assertTrue(RingService.isRinging)
        }
        requireNotNull(service).get().onStartCommand(
            Intent(context, RingService::class.java).setAction(AlarmIntents.ACTION_DISMISS), 0, 2)
        assertFalse(RingService.isRinging)
        assertTrue(AlarmMirror.loadSnoozes(context).isEmpty())
        assertTrue(shadowOf(manager).scheduledAlarms.isEmpty())
    }

    @Test
    fun logoutCancelsSnoozesAndRejectsQueuedDeliveryAndSnoozeRequests() {
        val generation = AlarmMirror.generation(context)
        scheduler.snooze(alarm(), generation)
        val scheduled = shadowOf(manager).scheduledAlarms.single()
        val queued = Intent(shadowOf(scheduled.operation).savedIntent)

        AlarmMirror.clear(context)
        AlarmReceiver().onReceive(context, queued)

        assertTrue(shadowOf(manager).scheduledAlarms.isEmpty())
        assertTrue(AlarmMirror.loadSnoozes(context).isEmpty())
        assertNull(shadowOf(context).nextStartedService)
        assertThrows(IllegalStateException::class.java) { scheduler.snooze(alarm(), generation) }
        assertTrue(AlarmMirror.loadSnoozes(context).isEmpty())
    }

    @Test
    fun cancelAllCancelsSnoozes() {
        scheduler.snooze(alarm(), AlarmMirror.generation(context))
        scheduler.cancelAll()
        assertTrue(shadowOf(manager).scheduledAlarms.isEmpty())
        assertTrue(AlarmMirror.loadSnoozes(context).isEmpty())
        boot()
        assertTrue(shadowOf(manager).scheduledAlarms.isEmpty())
    }

    @Test
    fun duplicateSnoozeDeliveryCannotRingTwice() {
        scheduler.snooze(alarm(), AlarmMirror.generation(context))
        val scheduled = shadowOf(manager).scheduledAlarms.single()
        val delivery = Intent(shadowOf(scheduled.operation).savedIntent)
        manager.cancel(requireNotNull(scheduled.operation))
        AlarmReceiver().onReceive(context, delivery)
        assertNotNull(shadowOf(context).nextStartedService)
        AlarmReceiver().onReceive(context, delivery)
        assertNull(shadowOf(context).nextStartedService)
    }

    @Test
    fun widgetShowsSnoozeBeforeALaterPlannedAlarm() {
        val now = System.currentTimeMillis()
        scheduler.replaceAll(listOf(alarm().copy(fireAtMillis = now + 3_600_000L)))
        scheduler.snooze(alarm(), AlarmMirror.generation(context))
        val snooze = AlarmMirror.loadSnoozes(context).single().alarm
        val zone = TimeZone.getTimeZone("UTC")
        assertEquals(ClipperClockText.formatNextAlarm(snooze.fireAtMillis, now, zone),
            ClipperClockText.nextAlarm(AlarmMirror.loadUpcoming(context), now, zone))
    }

    @Test
    fun snoozeWithoutARingingAlarmDoesNothing() {
        service = Robolectric.buildService(RingService::class.java).create()
        requireNotNull(service).get().onStartCommand(
            Intent(context, RingService::class.java).setAction(AlarmIntents.ACTION_SNOOZE), 0, 1)
        assertFalse(RingService.isRinging)
        assertTrue(AlarmMirror.loadSnoozes(context).isEmpty())
    }

    @Test
    fun normalPlanAlarmStillRings() {
        scheduler.replaceAll(listOf(alarm().copy(fireAtMillis = System.currentTimeMillis() + 60_000L)))
        val scheduled = shadowOf(manager).scheduledAlarms.single()
        manager.cancel(requireNotNull(scheduled.operation))
        AlarmReceiver().onReceive(context, shadowOf(scheduled.operation).savedIntent)
        val request = requireNotNull(shadowOf(context).nextStartedService)
        service = Robolectric.buildService(RingService::class.java).create()
        requireNotNull(service).get().onStartCommand(request, 0, 1)
        assertTrue(RingService.isRinging)
    }

    @Test
    fun queuedRingRequestCannotStartAfterLogout() {
        RingService.start(context, alarm(), AlarmMirror.generation(context))
        val queued = requireNotNull(shadowOf(context).nextStartedService)
        AlarmMirror.clear(context)
        service = Robolectric.buildService(RingService::class.java).create()
        requireNotNull(service).get().onStartCommand(queued, 0, 1)
        assertFalse(RingService.isRinging)
        assertTrue(AlarmMirror.loadSnoozes(context).isEmpty())
    }

    private fun startRinging(alarm: PlannedAlarm) {
        service = Robolectric.buildService(RingService::class.java).create()
        RingService.start(context, alarm, AlarmMirror.generation(context))
        requireNotNull(service).get().onStartCommand(shadowOf(context).nextStartedService, 0, 1)
        assertTrue(RingService.isRinging)
    }

    private fun boot() {
        context.sendBroadcast(Intent(context, BootReceiver::class.java)
            .setAction(Intent.ACTION_LOCKED_BOOT_COMPLETED))
        shadowOf(Looper.getMainLooper()).idle()
    }

    private fun slider(view: View): AlarmSlideControl? = when (view) {
        is AlarmSlideControl -> view
        is ViewGroup -> (0 until view.childCount).firstNotNullOfOrNull { slider(view.getChildAt(it)) }
        else -> null
    }

    private fun drag(slider: AlarmSlideControl, fraction: Float) {
        val density = slider.resources.displayMetrics.density
        slider.layout(0, 0, (400 * density).toInt(), (96 * density).toInt())
        val now = SystemClock.uptimeMillis()
        listOf(MotionEvent.ACTION_DOWN to 0.5f, MotionEvent.ACTION_MOVE to fraction,
            MotionEvent.ACTION_UP to fraction).forEach { (action, position) ->
            val event = MotionEvent.obtain(now, now, action, slider.width * position, slider.height / 2f, 0)
            try {
                slider.dispatchTouchEvent(event)
            } finally {
                event.recycle()
            }
        }
    }

    private fun alarm(itemId: String = "item") = PlannedAlarm(
        itemId, "occurrence", "Morning", System.currentTimeMillis(), System.currentTimeMillis())
}
