package com.clipper.editor

import android.content.Context
import android.graphics.Color
import android.graphics.Typeface
import android.os.Build
import android.text.Editable
import android.text.InputType
import android.text.TextWatcher
import android.view.Gravity
import android.view.inputmethod.EditorInfo
import android.widget.EditText
import expo.modules.kotlin.AppContext
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import expo.modules.kotlin.records.Field
import expo.modules.kotlin.records.Record
import expo.modules.kotlin.viewevent.EventDispatcher
import expo.modules.kotlin.views.ExpoView

class InputState : Record {
    @Field var text: String = ""
    @Field var revision: Int = 0
    @Field var eventCount: Int = 0
    @Field var start: Int = 0
    @Field var end: Int = 0
}

class InputColors : Record {
    @Field var text: String = ""
    @Field var background: String = ""
    @Field var selection: String = ""
    @Field var cursor: String = ""
}

class ClipperEditorModule : Module() {
    override fun definition() = ModuleDefinition {
        Name("ClipperEditor")
        View(ClipperEditorView::class) {
            Events("onEdit")
            Prop("state") { view: ClipperEditorView, state: InputState -> view.apply(state) }
            Prop("editable") { view: ClipperEditorView, editable: Boolean ->
                view.input.isEnabled = editable
            }
            Prop("colors") { view: ClipperEditorView, colors: InputColors ->
                view.input.setTextColor(Color.parseColor(colors.text))
                view.input.setBackgroundColor(Color.parseColor(colors.background))
                view.input.highlightColor = Color.parseColor(colors.selection)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    view.input.textCursorDrawable?.setTint(Color.parseColor(colors.cursor))
                }
            }
        }
    }
}

class ClipperEditorView(context: Context, appContext: AppContext) : ExpoView(context, appContext) {
    override val shouldUseAndroidLayout = true
    private val onEdit by EventDispatcher()
    private var revision = 0
    private var eventCount = 0
    private var applying = true
    internal val input = object : EditText(context) {
        override fun onSelectionChanged(start: Int, end: Int) {
            super.onSelectionChanged(start, end)
            if (!applying) emit(false)
        }
    }

    init {
        input.layoutParams = LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT)
        input.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or
            InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        input.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI
        input.gravity = Gravity.TOP or Gravity.START
        input.typeface = Typeface.MONOSPACE
        input.textSize = 13f
        input.setLineSpacing(5 * resources.displayMetrics.scaledDensity, 1f)
        val padding = (12 * resources.displayMetrics.density).toInt()
        input.setPadding(padding, padding, padding, padding)
        input.isVerticalScrollBarEnabled = true
        input.contentDescription = "Document text"
        input.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) = Unit
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) = Unit
            override fun afterTextChanged(s: Editable?) {
                if (!applying) emit(false)
            }
        })
        addView(input)
        applying = false
    }

    fun apply(state: InputState) {
        if (state.eventCount != eventCount || state.revision < revision) return
        applying = true
        try {
            if (input.text.toString() != state.text) {
                val old = input.text.toString()
                var start = 0
                while (start < old.length && start < state.text.length && old[start] == state.text[start]) start++
                var end = old.length
                var nextEnd = state.text.length
                while (end > start && nextEnd > start && old[end - 1] == state.text[nextEnd - 1]) {
                    end--
                    nextEnd--
                }
                input.text.replace(start, end, state.text.substring(start, nextEnd))
            }
            input.setSelection(state.start.coerceIn(0, input.length()), state.end.coerceIn(0, input.length()))
            revision = state.revision
        } finally {
            applying = false
        }
        emit(true)
    }

    private fun emit(applied: Boolean) {
        if (!applied) eventCount++
        onEdit(mapOf(
            "revision" to revision,
            "eventCount" to eventCount,
            "text" to input.text.toString(),
            "start" to input.selectionStart.coerceAtLeast(0),
            "end" to input.selectionEnd.coerceAtLeast(0),
            "applied" to applied,
        ))
    }
}
