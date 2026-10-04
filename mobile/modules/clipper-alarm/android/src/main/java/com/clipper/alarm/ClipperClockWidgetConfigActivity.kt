package com.clipper.alarm

import android.app.Activity
import android.app.AlertDialog
import android.appwidget.AppWidgetManager
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.text.Editable
import android.text.TextWatcher
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.ScrollView
import android.widget.TextView
import java.util.Locale
import java.util.TimeZone

class ClipperClockWidgetConfigActivity : Activity() {
    private var appWidgetId = AppWidgetManager.INVALID_APPWIDGET_ID
    private lateinit var firstZone: ZoneEditor
    private lateinit var secondZone: ZoneEditor

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setResult(RESULT_CANCELED)
        appWidgetId = intent.getIntExtra(AppWidgetManager.EXTRA_APPWIDGET_ID,
            AppWidgetManager.INVALID_APPWIDGET_ID)
        if (appWidgetId == AppWidgetManager.INVALID_APPWIDGET_ID) {
            finish()
            return
        }
        val config = ClipperClockWidgetPrefs.load(this, appWidgetId)
        firstZone = ZoneEditor(R.string.clipper_clock_first_zone,
            savedInstanceState?.getString("first_zone") ?: config.zoneOneId,
            savedInstanceState?.getString("first_label") ?: config.zoneOneLabel)
        secondZone = ZoneEditor(R.string.clipper_clock_second_zone,
            savedInstanceState?.getString("second_zone") ?: config.zoneTwoId,
            savedInstanceState?.getString("second_label") ?: config.zoneTwoLabel)
        if (savedInstanceState != null) {
            firstZone.selectZone(savedInstanceState.getString("first_zone"), resetLabel = false)
            secondZone.selectZone(savedInstanceState.getString("second_zone"), resetLabel = false)
        }
        val content = column().apply {
            setPadding(dp(20), dp(24), dp(20), dp(24))
            addView(TextView(this@ClipperClockWidgetConfigActivity).apply {
                setText(R.string.clipper_clock_setup)
                textSize = 24f
            })
            addView(TextView(this@ClipperClockWidgetConfigActivity).apply {
                setText(R.string.clipper_clock_instructions)
                setPadding(0, dp(16), 0, dp(8))
            })
            addView(firstZone.layout)
            addView(secondZone.layout)
            addView(Button(this@ClipperClockWidgetConfigActivity).apply {
                setText(R.string.clipper_clock_save)
                setOnClickListener { save() }
            })
            addView(Button(this@ClipperClockWidgetConfigActivity).apply {
                setText(R.string.clipper_clock_cancel)
                setOnClickListener { finish() }
            })
        }
        setContentView(ScrollView(this).apply {
            addView(content)
            setOnApplyWindowInsetsListener { view, insets ->
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    val bars = insets.getInsets(WindowInsets.Type.systemBars())
                    view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
                }
                insets
            }
        })
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putString("first_zone", firstZone.zoneId)
        outState.putString("first_label", firstZone.label.text.toString())
        outState.putString("second_zone", secondZone.zoneId)
        outState.putString("second_label", secondZone.label.text.toString())
        super.onSaveInstanceState(outState)
    }

    private fun save() {
        ClipperClockWidgetPrefs.save(this, appWidgetId, ClipperClockWidgetConfig(
            firstZone.zoneId, firstZone.savedLabel(), secondZone.zoneId, secondZone.savedLabel()))
        ClipperClockWidget.updateAll(this)
        setResult(RESULT_OK, Intent().putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, appWidgetId))
        finish()
    }

    private inner class ZoneEditor(title: Int, initialZone: String?, initialLabel: String) {
        var zoneId: String? = initialZone
            private set
        val label = EditText(this@ClipperClockWidgetConfigActivity).apply {
            setHint(R.string.clipper_clock_label)
            setSingleLine(true)
            setText(initialLabel)
        }
        private val choose = Button(this@ClipperClockWidgetConfigActivity).apply {
            setOnClickListener { chooseZone(zoneId) { selectZone(it) } }
        }
        val layout = column().apply {
            setPadding(0, dp(16), 0, dp(16))
            addView(TextView(this@ClipperClockWidgetConfigActivity).apply { setText(title) })
            addView(choose)
            addView(label)
        }

        init {
            selectZone(initialZone, resetLabel = false)
        }

        fun selectZone(id: String?, resetLabel: Boolean = true) {
            zoneId = id
            choose.text = id?.let { "$it (${defaultZoneLabel(it)})" }
                ?: getString(R.string.clipper_clock_none)
            label.visibility = if (id == null) View.GONE else View.VISIBLE
            if (resetLabel) label.setText(id?.let(::defaultZoneLabel).orEmpty())
        }

        fun savedLabel(): String = zoneId?.let {
            label.text.toString().trim().ifBlank { defaultZoneLabel(it) }
        }.orEmpty()
    }

    private data class ZoneChoice(val id: String?, val title: String)

    private fun chooseZone(selected: String?, onSelect: (String?) -> Unit) {
        val choices = listOf(ZoneChoice(null, getString(R.string.clipper_clock_none))) +
            TimeZone.getAvailableIDs().sortedBy(::defaultZoneLabel).map {
                ZoneChoice(it, "${defaultZoneLabel(it)} - $it")
            }
        var filtered = choices
        val search = EditText(this).apply {
            setHint(R.string.clipper_clock_search)
            setSingleLine(true)
        }
        val adapter = ArrayAdapter(this, android.R.layout.simple_list_item_single_choice,
            choices.map { it.title }.toMutableList())
        val list = ListView(this).apply {
            choiceMode = ListView.CHOICE_MODE_SINGLE
            this.adapter = adapter
            setItemChecked(choices.indexOfFirst { it.id == selected }, true)
        }
        val content = column().apply {
            setPadding(dp(16), 0, dp(16), 0)
            addView(search)
            addView(list, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(360)))
        }
        val dialog = AlertDialog.Builder(this)
            .setTitle(R.string.clipper_clock_zone)
            .setView(content)
            .setNegativeButton(R.string.clipper_clock_cancel, null)
            .create()
        list.setOnItemClickListener { _, _, position, _ ->
            onSelect(filtered[position].id)
            dialog.dismiss()
        }
        search.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {
                val query = s.toString().trim().lowercase(Locale.getDefault())
                filtered = choices.filter { it.title.lowercase(Locale.getDefault()).contains(query) }
                adapter.clear()
                adapter.addAll(filtered.map { it.title })
                list.clearChoices()
                val index = filtered.indexOfFirst { it.id == selected }
                if (index >= 0) list.setItemChecked(index, true)
            }
            override fun afterTextChanged(s: Editable?) {}
        })
        dialog.show()
    }

    private fun column() = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.WRAP_CONTENT)
    }

    private fun dp(value: Int): Int = (value * resources.displayMetrics.density).toInt()
}
