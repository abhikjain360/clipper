package com.clipper.alarm

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.os.Bundle
import android.util.TypedValue
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.view.accessibility.AccessibilityNodeInfo
import android.view.animation.DecelerateInterpolator
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.min

class AlarmSlideControl(context: Context) : View(context) {
    var onSnooze: () -> Unit = {}
    var onDismiss: () -> Unit = {}
    var canSnooze: Boolean = true
        set(value) {
            field = value
            contentDescription = context.getString(if (value) R.string.clipper_alarm_slide_description
                else R.string.clipper_alarm_dismiss_slide_description)
            reset()
        }

    private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val touchSlop = ViewConfiguration.get(context).scaledTouchSlop
    private var offset = 0f
    private var downX = 0f
    private var startOffset = 0f
    private var tracking = false
    private var dragged = false
    private var returnAnimation: ValueAnimator? = null

    init {
        isFocusable = true
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_YES
        contentDescription = context.getString(R.string.clipper_alarm_slide_description)
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        setMeasuredDimension(resolveSize(dp(320).toInt(), widthMeasureSpec),
            resolveSize(dp(96).toInt(), heightMeasureSpec))
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        reset()
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        val centerY = height / 2f
        val radius = handleRadius()
        val trackHeight = min(dp(88), height.toFloat())
        val progress = if (travel() > 0f) offset / travel() else 0f
        paint.style = Paint.Style.FILL
        paint.color = Color.parseColor("#21262d")
        canvas.drawRoundRect(0f, centerY - trackHeight / 2f, width.toFloat(),
            centerY + trackHeight / 2f, trackHeight / 2f, trackHeight / 2f, paint)

        paint.textSize = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, 16f, resources.displayMetrics)
        val baseline = centerY - (paint.ascent() + paint.descent()) / 2f
        if (canSnooze) {
            paint.textAlign = Paint.Align.LEFT
            paint.color = blend(Color.parseColor("#8b949e"), Color.WHITE, (-progress).coerceAtLeast(0f))
            canvas.drawText(context.getString(R.string.clipper_alarm_snooze), dp(20), baseline, paint)
        }
        paint.textAlign = Paint.Align.RIGHT
        paint.color = blend(Color.parseColor("#8b949e"), Color.WHITE, progress.coerceAtLeast(0f))
        canvas.drawText(context.getString(R.string.clipper_alarm_dismiss), width - dp(20), baseline, paint)

        val handleX = width / 2f + offset
        paint.color = blend(Color.parseColor("#30363d"), Color.parseColor("#e6edf3"),
            maxOf(abs(progress), if (isPressed) 0.2f else 0f))
        canvas.drawCircle(handleX, centerY, radius, paint)
        paint.color = if (abs(progress) >= THRESHOLD) Color.parseColor("#101214") else Color.WHITE
        paint.style = Paint.Style.STROKE
        paint.strokeWidth = dp(2)
        paint.strokeCap = Paint.Cap.ROUND
        if (canSnooze) {
            canvas.drawLine(handleX - dp(4), centerY - dp(6), handleX - dp(10), centerY, paint)
            canvas.drawLine(handleX - dp(10), centerY, handleX - dp(4), centerY + dp(6), paint)
        }
        canvas.drawLine(handleX + dp(4), centerY - dp(6), handleX + dp(10), centerY, paint)
        canvas.drawLine(handleX + dp(10), centerY, handleX + dp(4), centerY + dp(6), paint)
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (!isEnabled) return false
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                if (returnAnimation?.isRunning == true) return false
                val distance = hypot(event.x - (width / 2f + offset), event.y - height / 2f)
                if (event.pointerCount != 1 || distance > handleRadius() || travel() <= 0f) return false
                returnAnimation?.cancel()
                downX = event.x
                startOffset = offset
                tracking = true
                dragged = false
                isPressed = true
                parent?.requestDisallowInterceptTouchEvent(true)
                invalidate()
                return true
            }
            MotionEvent.ACTION_MOVE -> {
                if (!tracking) return false
                if (event.pointerCount != 1) {
                    returnToMiddle()
                    return true
                }
                val distance = event.x - downX
                if (abs(distance) > touchSlop) dragged = true
                offset = (startOffset + distance).coerceIn(if (canSnooze) -travel() else 0f, travel())
                invalidate()
                return true
            }
            MotionEvent.ACTION_UP -> {
                if (!tracking) return false
                offset = (startOffset + event.x - downX).coerceIn(if (canSnooze) -travel() else 0f, travel())
                val action = if (dragged && abs(offset) >= travel() * THRESHOLD) {
                    if (offset < 0f) onSnooze else onDismiss
                } else null
                returnToMiddle()
                action?.invoke()
                return true
            }
            MotionEvent.ACTION_CANCEL, MotionEvent.ACTION_POINTER_DOWN -> {
                if (!tracking) return false
                returnToMiddle()
                return true
            }
        }
        return tracking
    }

    override fun onInitializeAccessibilityNodeInfo(info: AccessibilityNodeInfo) {
        super.onInitializeAccessibilityNodeInfo(info)
        if (canSnooze) {
            info.addAction(AccessibilityNodeInfo.AccessibilityAction(R.id.clipper_alarm_snooze_action,
                context.getString(R.string.clipper_alarm_snooze)))
        }
        info.addAction(AccessibilityNodeInfo.AccessibilityAction(R.id.clipper_alarm_dismiss_action,
            context.getString(R.string.clipper_alarm_dismiss)))
    }

    override fun performAccessibilityAction(action: Int, arguments: Bundle?): Boolean {
        if (!isEnabled) return false
        when (action) {
            R.id.clipper_alarm_snooze_action -> {
                if (!canSnooze) return false
                reset()
                onSnooze()
                return true
            }
            R.id.clipper_alarm_dismiss_action -> {
                reset()
                onDismiss()
                return true
            }
        }
        return super.performAccessibilityAction(action, arguments)
    }

    override fun onDetachedFromWindow() {
        reset()
        super.onDetachedFromWindow()
    }

    private fun returnToMiddle() {
        tracking = false
        dragged = false
        isPressed = false
        parent?.requestDisallowInterceptTouchEvent(false)
        returnAnimation?.cancel()
        returnAnimation = ValueAnimator.ofFloat(offset, 0f).apply {
            duration = 220L
            interpolator = DecelerateInterpolator()
            addUpdateListener {
                offset = it.animatedValue as Float
                invalidate()
            }
            start()
        }
    }

    private fun reset() {
        returnAnimation?.cancel()
        returnAnimation = null
        tracking = false
        dragged = false
        isPressed = false
        offset = 0f
        parent?.requestDisallowInterceptTouchEvent(false)
        invalidate()
    }

    private fun handleRadius(): Float = min(dp(32), (height / 2f - dp(8)).coerceAtLeast(0f))

    private fun travel(): Float = (width / 2f - handleRadius() - dp(8)).coerceAtLeast(0f)

    private fun dp(value: Int): Float = value * resources.displayMetrics.density

    private fun blend(from: Int, to: Int, amount: Float): Int {
        val fraction = amount.coerceIn(0f, 1f)
        fun channel(start: Int, end: Int) = (start + (end - start) * fraction).toInt()
        return Color.rgb(channel(Color.red(from), Color.red(to)),
            channel(Color.green(from), Color.green(to)), channel(Color.blue(from), Color.blue(to)))
    }

    private companion object {
        const val THRESHOLD = 0.75f
    }
}
