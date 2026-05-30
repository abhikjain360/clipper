package com.clipper.clipper_app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import io.flutter.FlutterInjector
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.dart.DartExecutor
import io.flutter.plugin.common.MethodChannel

class SyncForegroundService : Service() {
    private var flutterEngine: FlutterEngine? = null
    private var serviceChannel: MethodChannel? = null
    private var foregroundStarted = false

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSyncService()
            return START_NOT_STICKY
        }

        startForegroundWithNotification()
        ensureFlutterEngine()
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onTimeout(startId: Int) {
        stopSyncService()
    }

    override fun onTimeout(startId: Int, fgsType: Int) {
        stopSyncService()
    }

    override fun onDestroy() {
        removeForegroundNotification()
        serviceChannel?.setMethodCallHandler(null)
        serviceChannel = null
        flutterEngine?.destroy()
        flutterEngine = null
        super.onDestroy()
    }

    private fun ensureFlutterEngine() {
        if (flutterEngine != null) return

        val flutterLoader = FlutterInjector.instance().flutterLoader()
        if (!flutterLoader.initialized()) {
            flutterLoader.startInitialization(applicationContext)
            flutterLoader.ensureInitializationComplete(applicationContext, null)
        }

        val engine = FlutterEngine(applicationContext)
        val channel = MethodChannel(
            engine.dartExecutor.binaryMessenger,
            "com.clipper.app/background_sync_service"
        )
        channel.setMethodCallHandler { call, result ->
            when (call.method) {
                "stopSelf" -> {
                    stopSyncService()
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }

        engine.dartExecutor.executeDartEntrypoint(
            DartExecutor.DartEntrypoint(
                flutterLoader.findAppBundlePath(),
                BACKGROUND_SYNC_ENTRYPOINT
            )
        )

        flutterEngine = engine
        serviceChannel = channel
    }

    private fun startForegroundWithNotification() {
        val notification = buildNotification()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
        foregroundStarted = true
    }

    private fun buildNotification(): Notification {
        val contentIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java)
                .setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            pendingIntentFlags()
        )
        val stopIntent = PendingIntent.getService(
            this,
            1,
            Intent(this, SyncForegroundService::class.java).setAction(ACTION_STOP),
            pendingIntentFlags()
        )

        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }

        return builder
            .setSmallIcon(R.drawable.ic_sync_notification)
            .setContentTitle("Clipper is syncing")
            .setContentText("Clipboard and files stay up to date in the background.")
            .setContentIntent(contentIntent)
            .setOngoing(true)
            .setShowWhen(false)
            .setCategory(Notification.CATEGORY_SERVICE)
            .setPriority(Notification.PRIORITY_LOW)
            .addAction(
                Notification.Action.Builder(
                    R.drawable.ic_sync_notification,
                    "Stop",
                    stopIntent
                ).build()
            )
            .build()
    }

    private fun pendingIntentFlags(): Int {
        val mutableFlag = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            PendingIntent.FLAG_IMMUTABLE
        } else {
            0
        }
        return PendingIntent.FLAG_UPDATE_CURRENT or mutableFlag
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return

        val channel = NotificationChannel(
            CHANNEL_ID,
            "Background sync",
            NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "Shows when Clipper is syncing in the background."
            setShowBadge(false)
        }

        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(channel)
    }

    private fun stopSyncService() {
        removeForegroundNotification()
        stopSelf()
    }

    private fun removeForegroundNotification() {
        if (!foregroundStarted) return

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } else {
            @Suppress("DEPRECATION")
            stopForeground(true)
        }
        foregroundStarted = false
    }

    companion object {
        private const val ACTION_START = "com.clipper.clipper_app.action.START_SYNC"
        private const val ACTION_STOP = "com.clipper.clipper_app.action.STOP_SYNC"
        private const val BACKGROUND_SYNC_ENTRYPOINT = "backgroundSyncMain"
        private const val CHANNEL_ID = "clipper_background_sync"
        private const val NOTIFICATION_ID = 1001

        fun start(context: Context) {
            val intent = Intent(context, SyncForegroundService::class.java).setAction(ACTION_START)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, SyncForegroundService::class.java))
        }
    }
}
