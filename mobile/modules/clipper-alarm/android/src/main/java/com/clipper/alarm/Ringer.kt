package com.clipper.alarm

import android.content.Context
import android.media.AudioAttributes
import android.media.MediaPlayer
import android.media.RingtoneManager
import android.net.Uri
import android.os.Build
import android.os.UserManager
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.provider.Settings
import android.util.Log

/** Plays alarm audio and vibration, including while credential storage is locked. */
class Ringer(private val context: Context) {

    private var player: MediaPlayer? = null
    private var playbackGeneration = 0

    private val alarmAudioAttributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_ALARM)
        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
        .build()

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
        // Invalidate callbacks before releasing: a late prepare/error callback
        // must not start a fallback after the user has dismissed the alarm.
        playbackGeneration += 1
        player?.let(::releasePlayer)
        player = null
        runCatching { vibrator?.cancel() }
    }

    private fun startSound() {
        playbackGeneration += 1
        val generation = playbackGeneration
        player?.let(::releasePlayer)
        player = null

        val userManager = context.getSystemService(UserManager::class.java)
        if (userManager?.isUserUnlocked != true) {
            // APK resources are available during Direct Boot; ringtone-provider
            // URIs generally are not until credential storage is unlocked.
            startBundledPlayer(generation)
            return
        }

        val preferred = RingtoneManager
            .getActualDefaultRingtoneUri(context, RingtoneManager.TYPE_ALARM)
            ?: Settings.System.DEFAULT_ALARM_ALERT_URI
        startPreferredPlayer(preferred, generation)
    }

    private fun startPreferredPlayer(uri: Uri, generation: Int) {
        val next = MediaPlayer()
        try {
            next.setAudioAttributes(alarmAudioAttributes)
            next.isLooping = true
            next.setOnPreparedListener { prepared ->
                if (!isCurrent(prepared, generation)) {
                    releasePlayer(prepared)
                    return@setOnPreparedListener
                }
                runCatching { prepared.start() }.onFailure { error ->
                    fallbackFromPreferred(prepared, uri, generation, error)
                }
            }
            next.setOnErrorListener { failed, what, extra ->
                fallbackFromPreferred(
                    failed,
                    uri,
                    generation,
                    IllegalStateException("MediaPlayer error what=$what extra=$extra"),
                )
                true
            }
            next.setDataSource(context, uri)
            if (generation != playbackGeneration) {
                releasePlayer(next)
                return
            }
            player = next
            next.prepareAsync()
        } catch (error: Exception) {
            releasePlayer(next)
            if (generation == playbackGeneration) {
                Log.w(TAG, "Could not prepare preferred alarm sound from $uri", error)
                startBundledPlayer(generation)
            }
        }
    }

    private fun fallbackFromPreferred(
        failed: MediaPlayer,
        uri: Uri,
        generation: Int,
        error: Throwable,
    ) {
        val shouldFallback = isCurrent(failed, generation)
        if (player === failed) player = null
        releasePlayer(failed)
        if (shouldFallback) {
            Log.w(TAG, "Preferred alarm sound failed from $uri; using bundled sound", error)
            startBundledPlayer(generation)
        }
    }

    private fun startBundledPlayer(generation: Int) {
        if (generation != playbackGeneration) return

        val next = runCatching {
            MediaPlayer.create(
                context,
                R.raw.clipper_alarm_fallback,
                alarmAudioAttributes,
                0,
            ) ?: error("MediaPlayer could not open bundled alarm sound")
        }.getOrElse { error ->
            Log.e(TAG, "Could not load bundled alarm sound", error)
            return
        }

        if (generation != playbackGeneration) {
            releasePlayer(next)
            return
        }
        next.isLooping = true
        next.setOnErrorListener { failed, what, extra ->
            val isActive = isCurrent(failed, generation)
            if (player === failed) player = null
            releasePlayer(failed)
            if (isActive) {
                Log.e(TAG, "Bundled alarm playback failed: what=$what extra=$extra")
            }
            true
        }
        player = next
        runCatching {
            next.start()
            Log.i(TAG, "Started bundled alarm sound")
        }.onFailure { error ->
            if (player === next) player = null
            releasePlayer(next)
            if (generation == playbackGeneration) {
                Log.e(TAG, "Could not start bundled alarm sound", error)
            }
        }
    }

    private fun isCurrent(candidate: MediaPlayer, generation: Int): Boolean =
        generation == playbackGeneration && player === candidate

    private fun releasePlayer(candidate: MediaPlayer) {
        runCatching { candidate.setOnPreparedListener(null) }
        runCatching { candidate.setOnErrorListener(null) }
        runCatching { if (candidate.isPlaying) candidate.stop() }
        runCatching { candidate.release() }
    }

    @Suppress("DEPRECATION")
    private fun startVibration() {
        // Wait 0ms, buzz 600ms, pause 600ms, repeating from index 0 until stopped.
        runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                vibrator?.vibrate(VibrationEffect.createWaveform(longArrayOf(0, 600, 600), 0))
            } else {
                vibrator?.vibrate(longArrayOf(0, 600, 600), 0)
            }
        }
    }

    private companion object {
        const val TAG = "ClipperAlarm"
    }
}
