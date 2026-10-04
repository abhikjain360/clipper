package com.clipper.alarm

import android.app.ActivityManager
import android.app.AlarmManager
import android.app.Application
import android.app.Notification
import android.content.Intent
import android.media.MediaPlayer
import android.os.Process
import android.os.UserManager
import android.os.Vibrator
import android.view.View
import android.view.ViewGroup
import android.widget.TextView
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.android.controller.ActivityController
import org.robolectric.android.controller.ServiceController
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowAlarmManager
import org.robolectric.shadows.ShadowMediaPlayer
import org.robolectric.shadows.ShadowPowerManager

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28], application = Application::class)
class RingServiceTest {
    private lateinit var context: Application
    private lateinit var scheduler: AlarmScheduler
    private lateinit var service: ServiceController<RingService>
    private var activity: ActivityController<RingActivity>? = null
    private var startId = 0
    private val players = mutableListOf<MediaPlayer>()

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        scheduler = AlarmScheduler(context)
        ShadowAlarmManager.setAutoSchedule(false)
        shadowOf(context.getSystemService(ActivityManager::class.java)).setProcesses(listOf(
            ActivityManager.RunningAppProcessInfo().apply {
                pid = Process.myPid()
                importance = ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND_SERVICE
            },
        ))
        shadowOf(context.getSystemService(UserManager::class.java)).setUserUnlocked(false)
        ShadowMediaPlayer.setMediaInfoProvider { ShadowMediaPlayer.MediaInfo(3_600_000, 0) }
        ShadowMediaPlayer.setCreateListener { player, _ -> players.add(player) }
        AlarmMirror.clear(context)
        service = Robolectric.buildService(RingService::class.java).create()
    }

    @After
    fun tearDown() {
        activity?.pause()?.stop()?.destroy()
        service.destroy()
        AlarmMirror.clear(context)
    }

    @Test
    fun timerWaitsForNormalAlarmAndKeepsRingingAfterSnooze() {
        val normal = normalAlarm()
        val timer = timer()
        deliver(normal)
        val player = players.single()
        val wakeLock = requireNotNull(ShadowPowerManager.getLatestWakeLock())
        openScreen()
        val oldSlider = slider()
        val oldDismiss = action("Dismiss")
        deliver(timer)

        assertDisplayed(normal)
        assertEquals(listOf("Snooze", "Dismiss"), actionTitles())
        assertTrue(slider().canSnooze)
        val snooze = action("Snooze")
        assertIdentity(normal, snooze)
        service.get().onStartCommand(snooze, 0, ++startId)

        assertDisplayed(timer)
        assertEquals(listOf("Dismiss"), actionTitles())
        assertFalse(slider().canSnooze)
        assertScreenLabel(timer.label)
        assertFalse(requireNotNull(activity).get().isFinishing)
        assertEquals(normal.itemId, AlarmMirror.loadSnoozes(context).single().alarm.itemId)
        assertKeepsRinging()
        assertSame(player, players.single())
        assertSame(wakeLock, ShadowPowerManager.getLatestWakeLock())

        assertIdentity(normal, oldDismiss)
        service.get().onStartCommand(oldDismiss, 0, ++startId)
        assertDisplayed(timer)
        oldSlider.onSnooze()
        handleRequest()
        assertEquals(1, AlarmMirror.loadSnoozes(context).size)
        assertKeepsRinging()

        val dismiss = action("Dismiss")
        assertIdentity(timer, dismiss)
        service.get().onStartCommand(dismiss, 0, ++startId)
        assertStopped()
        assertTrue(requireNotNull(activity).get().isFinishing)
    }

    @Test
    fun dismissingHiddenTimerLeavesNormalAlarmRinging() {
        val timer = timer()
        val normal = normalAlarm()
        deliver(timer)
        val dismissTimer = action("Dismiss")
        deliver(normal)

        assertDisplayed(normal)
        assertIdentity(timer, dismissTimer)
        service.get().onStartCommand(dismissTimer, 0, ++startId)
        assertDisplayed(normal)
        assertKeepsRinging()
        service.get().onStartCommand(action("Dismiss"), 0, ++startId)
        assertStopped()
    }

    @Test
    fun normalAlarmTakesTimerScreenAndTimerReturnsAfterDismissal() {
        val timer = timer()
        val normal = normalAlarm()
        deliver(timer)
        openScreen()
        assertFalse(slider().canSnooze)
        deliver(normal)

        assertDisplayed(normal)
        assertScreenLabel(normal.label)
        assertTrue(slider().canSnooze)
        slider().onDismiss()
        handleRequest()

        assertDisplayed(timer)
        assertScreenLabel(timer.label)
        assertFalse(slider().canSnooze)
        assertKeepsRinging()
    }

    @Test
    fun planRemovalStopsTimerAndLeavesNormalAlarmAlone() {
        val normal = normalAlarm()
        deliver(normal)
        deliver(timer())
        replacePlan(emptyList())

        assertDisplayed(normal)
        assertKeepsRinging()
        service.get().onStartCommand(action("Dismiss"), 0, ++startId)
        assertStopped()
    }

    @Test
    fun planKeepsTimerWithPastFireTimeAndRemovingLastTimerStopsRinging() {
        val timer = timer()
        deliver(timer)
        openScreen()
        assertTrue(timer.fireAtMillis < System.currentTimeMillis())
        replacePlan(listOf(timer))

        assertDisplayed(timer)
        assertKeepsRinging()
        assertEquals(timer, AlarmMirror.load(context).single())
        assertTrue(shadowOf(context.getSystemService(AlarmManager::class.java))
            .scheduledAlarms.isEmpty())

        replacePlan(emptyList())
        assertStopped()
        assertTrue(requireNotNull(activity).get().isFinishing)
    }

    private fun deliver(alarm: PlannedAlarm) {
        RingService.start(context, alarm, AlarmMirror.generation(context))
        handleRequest()
    }

    private fun replacePlan(plan: List<PlannedAlarm>) {
        scheduler.replaceAll(plan)
        val request = requireNotNull(shadowOf(context).nextStartedService)
        assertEquals(AlarmIntents.ACTION_PLAN_CHANGED, request.action)
        service.get().onStartCommand(request, 0, ++startId)
    }

    private fun handleRequest() {
        service.get().onStartCommand(requireNotNull(shadowOf(context).nextStartedService), 0, ++startId)
    }

    private fun openScreen() {
        activity = Robolectric.buildActivity(RingActivity::class.java,
            shadowOf(notification().fullScreenIntent).savedIntent).setup()
    }

    private fun notification(): Notification =
        requireNotNull(shadowOf(service.get()).lastForegroundNotification)

    private fun action(title: String): Intent =
        shadowOf(notification().actions.single { it.title.toString() == title }.actionIntent).savedIntent

    private fun actionTitles(): List<String> = notification().actions.map { it.title.toString() }

    private fun assertDisplayed(alarm: PlannedAlarm) {
        assertEquals(alarm, RingService.displayedAlarm)
        assertEquals(alarm.label, notification().extras.getString(Notification.EXTRA_TITLE))
        val intent = shadowOf(notification().fullScreenIntent).savedIntent
        assertIdentity(alarm, intent)
        assertEquals(alarm.canSnooze, intent.getBooleanExtra(AlarmIntents.EXTRA_CAN_SNOOZE, true))
    }

    private fun assertIdentity(alarm: PlannedAlarm, intent: Intent) {
        assertEquals(alarm.itemId, intent.getStringExtra(AlarmIntents.EXTRA_ITEM_ID))
        assertEquals(alarm.occurrenceKey, intent.getStringExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY))
        assertEquals(alarm.fireAtMillis, intent.getLongExtra(AlarmIntents.EXTRA_FIRE_AT, -1L))
    }

    private fun assertKeepsRinging() {
        assertTrue(RingService.isRinging)
        assertFalse(shadowOf(service.get()).isForegroundStopped)
        assertTrue(shadowOf(context.getSystemService(Vibrator::class.java)).isVibrating)
        assertTrue(requireNotNull(ShadowPowerManager.getLatestWakeLock()).isHeld)
        assertTrue(players.last().isPlaying)
        assertNull(shadowOf(context).nextStartedActivity)
    }

    private fun assertStopped() {
        assertFalse(RingService.isRinging)
        assertNull(RingService.displayedAlarm)
        assertTrue(shadowOf(service.get()).isForegroundStopped)
        assertFalse(shadowOf(context.getSystemService(Vibrator::class.java)).isVibrating)
        assertFalse(requireNotNull(ShadowPowerManager.getLatestWakeLock()).isHeld)
        assertFalse(shadowOf(players.last()).isReallyPlaying)
        assertNull(shadowOf(context).nextStartedActivity)
    }

    private fun slider(): AlarmSlideControl = views()
        .filterIsInstance<AlarmSlideControl>().single()

    private fun assertScreenLabel(label: String) {
        assertTrue(views().filterIsInstance<TextView>().any { it.text.toString() == label })
    }

    private fun views(): List<View> {
        fun descendants(view: View): List<View> = listOf(view) +
            if (view is ViewGroup) (0 until view.childCount).flatMap { descendants(view.getChildAt(it)) }
            else emptyList()
        return descendants(requireNotNull(activity).get().findViewById(android.R.id.content))
    }

    private fun normalAlarm(): PlannedAlarm = PlannedAlarm(
        "item", "occurrence", "Morning", System.currentTimeMillis(), System.currentTimeMillis())

    private fun timer(): PlannedAlarm = PlannedAlarm(
        "session", "timer:step:timer", "Check the oven", System.currentTimeMillis() - 5_000L,
        System.currentTimeMillis(), false)
}
