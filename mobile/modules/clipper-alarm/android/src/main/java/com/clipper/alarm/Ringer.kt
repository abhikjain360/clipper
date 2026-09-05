package com.clipper.alarm

import android.content.Context
import android.media.AudioAttributes
import android.media.MediaPlayer
import android.media.RingtoneManager
import android.net.Uri
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.provider.Settings
import android.util.Log

/**
 * Makes the noise.
 *
 * Two details are load-bearing. Audio goes out on `USAGE_ALARM`, which is what
 * lets it through the ringer setting and Do Not Disturb — an alarm on the media
 * stream is silent exactly when it matters. And vibration goes through
 * `VibratorManager`; the old `Vibrator` service is deprecated.
 */
class Ringer(private val context: Context) {

    private var player: MediaPlayer? = null

    /**
     * `VibratorManager` replaced the `Vibrator` service in API 31 and the old
     * one is deprecated, so both paths exist rather than one deprecated call.
     */
    @Suppress("DEPRECATION")
    private val vibrator: Vibrator? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            context.getSystemService(VibratorManager::class.java)?.defaultVibrator
        } else {
            context.getSystemService(Vibrator::class.java)
        }

    fun start(vibrate: Boolean) {
        startSound()
        if (vibrate) startVibration()
    }

    fun stop() {
        player?.let { active ->
            runCatching { if (active.isPlaying) active.stop() }
            runCatching { active.release() }
        }
        player = null
        runCatching { vibrator?.cancel() }
    }

    private fun startSound() {
        val preferred = RingtoneManager
            .getActualDefaultRingtoneUri(context, RingtoneManager.TYPE_ALARM)
            ?: Settings.System.DEFAULT_ALARM_ALERT_URI
        if (!startPlayer(preferred)) {
            // A device with no configured alarm sound still has to ring.
            startPlayer(Settings.System.DEFAULT_ALARM_ALERT_URI)
        }
    }

    private fun startPlayer(uri: Uri): Boolean = runCatching {
        val next = MediaPlayer().apply {
            setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ALARM)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build(),
            )
            setDataSource(context, uri)
            isLooping = true
            setOnPreparedListener { it.start() }
            prepareAsync()
        }
        player = next
        true
    }.getOrElse { error ->
        Log.w(TAG, "Could not start alarm sound from $uri", error)
        false
    }

    private fun startVibration() {
        // Wait 0ms, buzz 600ms, pause 600ms, repeating from index 0 until stopped.
        runCatching {
            vibrator?.vibrate(VibrationEffect.createWaveform(longArrayOf(0, 600, 600), 0))
        }
    }

    private companion object {
        const val TAG = "ClipperAlarm"
    }
}
