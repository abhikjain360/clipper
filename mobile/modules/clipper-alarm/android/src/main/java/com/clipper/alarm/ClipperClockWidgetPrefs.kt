package com.clipper.alarm

import android.content.Context
import android.content.SharedPreferences

data class ClipperClockWidgetConfig(
    val zoneOneId: String? = null,
    val zoneOneLabel: String = "",
    val zoneTwoId: String? = null,
    val zoneTwoLabel: String = "",
)

object ClipperClockWidgetPrefs {
    private const val NAME = "clipper_clock_widgets"
    private val fields = listOf("zone_one_id", "zone_one_label", "zone_two_id", "zone_two_label")

    fun load(context: Context, appWidgetId: Int): ClipperClockWidgetConfig {
        val prefs = prefs(context)
        return ClipperClockWidgetConfig(
            zoneOneId = prefs.getString("$appWidgetId.zone_one_id", null),
            zoneOneLabel = prefs.getString("$appWidgetId.zone_one_label", "").orEmpty(),
            zoneTwoId = prefs.getString("$appWidgetId.zone_two_id", null),
            zoneTwoLabel = prefs.getString("$appWidgetId.zone_two_label", "").orEmpty(),
        )
    }

    fun save(context: Context, appWidgetId: Int, config: ClipperClockWidgetConfig) {
        check(prefs(context).edit()
            .putString("$appWidgetId.zone_one_id", config.zoneOneId)
            .putString("$appWidgetId.zone_one_label", config.zoneOneLabel)
            .putString("$appWidgetId.zone_two_id", config.zoneTwoId)
            .putString("$appWidgetId.zone_two_label", config.zoneTwoLabel)
            .commit()) { "Could not save the clock widget" }
    }

    fun delete(context: Context, appWidgetId: Int) {
        val editor = prefs(context).edit()
        fields.forEach { editor.remove("$appWidgetId.$it") }
        check(editor.commit()) { "Could not delete the clock widget" }
    }

    private fun prefs(context: Context): SharedPreferences = context.applicationContext
        .createDeviceProtectedStorageContext()
        .getSharedPreferences(NAME, Context.MODE_PRIVATE)
}
