package com.clipper.alarm

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.text.format.DateFormat
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView
import java.util.Date

/**
 * The screen shown while an alarm rings.
 *
 * Plain Android views rather than React Native. This has to appear over the
 * lock screen after a reboot, before the user has unlocked, and at that point
 * the JS bundle, the encrypted store and the sync engine are all unavailable.
 *
 * `showWhenLocked` and `turnScreenOn` in the manifest are what put it in front
 * of the keyguard instead of behind it.
 */
class RingActivity : Activity() {

    private val onRingingStopped: () -> Unit = { finish() }
    private val onAlarmChanged: (PlannedAlarm) -> Unit = { showAlarm(it) }

    override fun onStart() {
        super.onStart()
        RingService.stoppedListeners.add(onRingingStopped)
        RingService.alarmListeners.add(onAlarmChanged)
        // Also handles a notification tap racing with dismissal/auto-silence.
        if (!RingService.isRinging) finish()
        else RingService.displayedAlarm?.let(::showAlarm)
    }

    override fun onStop() {
        RingService.stoppedListeners.remove(onRingingStopped)
        RingService.alarmListeners.remove(onAlarmChanged)
        super.onStop()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        showOverKeyguard()

        RingService.displayedAlarm?.let(::showAlarm)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        RingService.displayedAlarm?.let(::showAlarm)
    }

    private fun showAlarm(alarm: PlannedAlarm) {
        setIntent(intent(this, alarm))
        setContentView(buildLayout(alarm))
    }

    /**
     * Put this screen in front of the keyguard rather than behind it.
     *
     * The dedicated methods arrived in API 27. Anything older needs the
     * equivalent window flags, which newer releases deprecate, so there are
     * two paths rather than one call.
     */
    @Suppress("DEPRECATION")
    private fun showOverKeyguard() {
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            window.setDecorFitsSystemWindows(false)
        } else {
            window.decorView.systemUiVisibility = View.SYSTEM_UI_FLAG_LAYOUT_STABLE or
                View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        } else {
            window.addFlags(
                WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or
                    WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON,
            )
        }
    }

    @Suppress("DEPRECATION")
    private fun buildLayout(alarm: PlannedAlarm): ViewGroup {
        val density = resources.displayMetrics.density
        fun dp(value: Int) = (value * density).toInt()

        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setBackgroundColor(Color.parseColor("#101214"))
            setPadding(dp(24), dp(24), dp(24), dp(32))
            setOnApplyWindowInsetsListener { view, insets ->
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    val bars = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
                    view.setPadding(dp(24) + bars.left, dp(24) + bars.top,
                        dp(24) + bars.right, dp(32) + bars.bottom)
                } else {
                    view.setPadding(dp(24) + insets.systemWindowInsetLeft,
                        dp(24) + insets.systemWindowInsetTop,
                        dp(24) + insets.systemWindowInsetRight,
                        dp(32) + insets.systemWindowInsetBottom)
                }
                insets
            }

            val details = LinearLayout(this@RingActivity).apply {
                orientation = LinearLayout.VERTICAL
                gravity = Gravity.CENTER
                addView(TextView(this@RingActivity).apply {
                    text = DateFormat.getTimeFormat(this@RingActivity).format(Date())
                    textSize = 56f
                    setTextColor(Color.WHITE)
                    gravity = Gravity.CENTER
                })
                addView(TextView(this@RingActivity).apply {
                    text = alarm.label
                    textSize = 22f
                    setTextColor(Color.parseColor("#8b949e"))
                    gravity = Gravity.CENTER
                    setPadding(0, dp(12), 0, dp(24))
                })
            }
            addView(details, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(AlarmSlideControl(this@RingActivity).apply {
                canSnooze = alarm.canSnooze
                onSnooze = { RingService.snooze(this@RingActivity, alarm) }
                onDismiss = { RingService.dismiss(this@RingActivity, alarm) }
            }, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(96)))
        }
    }

    companion object {
        fun intent(
            context: Context,
            alarm: PlannedAlarm,
        ): Intent = Intent(context, RingActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            putExtra(AlarmIntents.EXTRA_LABEL, alarm.label)
            putExtra(AlarmIntents.EXTRA_ITEM_ID, alarm.itemId)
            putExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY, alarm.occurrenceKey)
            putExtra(AlarmIntents.EXTRA_FIRE_AT, alarm.fireAtMillis)
            putExtra(AlarmIntents.EXTRA_START, alarm.occurrenceStartMillis)
            putExtra(AlarmIntents.EXTRA_CAN_SNOOZE, alarm.canSnooze)
        }
    }
}
