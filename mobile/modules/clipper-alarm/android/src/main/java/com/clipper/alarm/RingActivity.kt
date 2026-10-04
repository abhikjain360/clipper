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
 * The label it shows arrives in the intent for the same reason.
 *
 * `showWhenLocked` and `turnScreenOn` in the manifest are what put it in front
 * of the keyguard instead of behind it.
 */
class RingActivity : Activity() {

    private val onRingingStopped: () -> Unit = { finish() }

    override fun onStart() {
        super.onStart()
        RingService.stoppedListeners.add(onRingingStopped)
        // Also handles a notification tap racing with dismissal/auto-silence.
        if (!RingService.isRinging) finish()
    }

    override fun onStop() {
        RingService.stoppedListeners.remove(onRingingStopped)
        super.onStop()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        showOverKeyguard()

        val label = intent.getStringExtra(AlarmIntents.EXTRA_LABEL) ?: "Alarm"
        setContentView(buildLayout(label))
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        setContentView(buildLayout(intent.getStringExtra(AlarmIntents.EXTRA_LABEL) ?: "Alarm"))
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
    private fun buildLayout(label: String): ViewGroup {
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
                    text = label
                    textSize = 22f
                    setTextColor(Color.parseColor("#8b949e"))
                    gravity = Gravity.CENTER
                    setPadding(0, dp(12), 0, dp(24))
                })
            }
            addView(details, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(AlarmSlideControl(this@RingActivity).apply {
                canSnooze = intent.getBooleanExtra(AlarmIntents.EXTRA_CAN_SNOOZE, true)
                onSnooze = { RingService.snooze(this@RingActivity) }
                onDismiss = { RingService.dismiss(this@RingActivity) }
            }, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(96)))
        }
    }

    companion object {
        fun intent(
            context: Context,
            label: String,
            itemId: String,
            occurrenceKey: String,
            canSnooze: Boolean = true,
        ): Intent = Intent(context, RingActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            putExtra(AlarmIntents.EXTRA_LABEL, label)
            putExtra(AlarmIntents.EXTRA_ITEM_ID, itemId)
            putExtra(AlarmIntents.EXTRA_OCCURRENCE_KEY, occurrenceKey)
            putExtra(AlarmIntents.EXTRA_CAN_SNOOZE, canSnooze)
        }
    }
}
