package com.clipper.alarm

import android.app.Application
import android.os.Looper
import android.os.SystemClock
import android.view.MotionEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28], application = Application::class)
class AlarmSlideControlTest {
    private lateinit var slider: AlarmSlideControl
    private val actions = mutableListOf<String>()

    @Before
    fun setUp() {
        slider = AlarmSlideControl(RuntimeEnvironment.getApplication()).apply {
            onSnooze = { actions.add("snooze") }
            onDismiss = { actions.add("dismiss") }
        }
        val density = slider.resources.displayMetrics.density
        slider.layout(0, 0, (400 * density).toInt(), (96 * density).toInt())
    }

    @Test
    fun draggingLeftPastTheThresholdSnoozesOnlyOnRelease() {
        touch(MotionEvent.ACTION_DOWN, 0.5f)
        touch(MotionEvent.ACTION_MOVE, 0.05f)
        assertTrue(actions.isEmpty())
        touch(MotionEvent.ACTION_UP, 0.05f)
        assertEquals(listOf("snooze"), actions)
    }

    @Test
    fun draggingRightPastTheThresholdDismissesOnlyOnRelease() {
        touch(MotionEvent.ACTION_DOWN, 0.5f)
        touch(MotionEvent.ACTION_MOVE, 0.95f)
        assertTrue(actions.isEmpty())
        touch(MotionEvent.ACTION_UP, 0.95f)
        assertEquals(listOf("dismiss"), actions)
    }

    @Test
    fun shortDragDoesNothingAndReturnsToTheMiddle() {
        touch(MotionEvent.ACTION_DOWN, 0.5f)
        touch(MotionEvent.ACTION_MOVE, 0.66f)
        touch(MotionEvent.ACTION_UP, 0.66f)
        assertTrue(actions.isEmpty())
        shadowOf(Looper.getMainLooper()).idleFor(300, TimeUnit.MILLISECONDS)
        touch(MotionEvent.ACTION_DOWN, 0.5f)
        touch(MotionEvent.ACTION_MOVE, 0.05f)
        touch(MotionEvent.ACTION_UP, 0.05f)
        assertEquals(listOf("snooze"), actions)
    }

    @Test
    fun tappingTheHandleAndTrackDoesNothing() {
        touch(MotionEvent.ACTION_DOWN, 0.05f)
        touch(MotionEvent.ACTION_UP, 0.05f)
        touch(MotionEvent.ACTION_DOWN, 0.5f)
        touch(MotionEvent.ACTION_UP, 0.5f)
        assertTrue(actions.isEmpty())
    }

    @Test
    fun aDragStartingOnTheTrackDoesNothing() {
        touch(MotionEvent.ACTION_DOWN, 0.05f)
        touch(MotionEvent.ACTION_MOVE, 0.95f)
        touch(MotionEvent.ACTION_UP, 0.95f)
        assertTrue(actions.isEmpty())
    }

    @Test
    fun cancellingADragPastTheThresholdDoesNothing() {
        touch(MotionEvent.ACTION_DOWN, 0.5f)
        touch(MotionEvent.ACTION_MOVE, 0.95f)
        touch(MotionEvent.ACTION_CANCEL, 0.95f)
        assertTrue(actions.isEmpty())
    }

    @Test
    fun accessibilityActionsSnoozeAndDismissWithoutDragging() {
        assertTrue(slider.performAccessibilityAction(R.id.clipper_alarm_snooze_action, null))
        assertTrue(slider.performAccessibilityAction(R.id.clipper_alarm_dismiss_action, null))
        assertEquals(listOf("snooze", "dismiss"), actions)
    }

    private fun touch(action: Int, fraction: Float) {
        val now = SystemClock.uptimeMillis()
        val event = MotionEvent.obtain(now, now, action, slider.width * fraction, slider.height / 2f, 0)
        try {
            slider.dispatchTouchEvent(event)
        } finally {
            event.recycle()
        }
    }
}
