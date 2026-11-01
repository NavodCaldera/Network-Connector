package lk.networkconnector.spike

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.SystemClock
import android.util.Log

/**
 * Foreground service (type specialUse) that runs one Wasm job and shows an ongoing
 * notification with a Stop action. Stop calls into Rust and must end the job within 1 s.
 */
class HelperService : Service() {

    companion object {
        const val TAG = "NcSpike"
        const val ACTION_START = "lk.networkconnector.spike.START"
        const val ACTION_STOP = "lk.networkconnector.spike.STOP"
        const val EXTRA_N = "n"
        private const val CHANNEL = "helper"
        private const val NOTIF_ID = 1

        /** Latest status line, read by MainActivity. Spike-level plumbing only. */
        @Volatile var status: String = "Idle"
        @Volatile var listener: ((String) -> Unit)? = null

        private fun publish(s: String) {
            status = s
            Log.i(TAG, s)
            listener?.invoke(s)
        }
    }

    @Volatile private var worker: Thread? = null
    @Volatile private var stopRequestedAt = 0L

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> requestStop()
            else -> startJob(intent?.getLongExtra(EXTRA_N, 300_000_000L) ?: 300_000_000L)
        }
        return START_NOT_STICKY
    }

    private fun startJob(n: Long) {
        goForeground()
        if (worker?.isAlive == true) return
        val t = Thread {
            val runtime = NativeBridge.runtimeName()
            publish("Running sum(0..$n) on $runtime")
            val t0 = SystemClock.elapsedRealtime()
            val r = NativeBridge.nativeRun(n)
            val t1 = SystemClock.elapsedRealtime()
            val line = when (r) {
                NativeBridge.STATUS_STOPPED ->
                    "RESULT runtime=$runtime n=$n stopped stop_latency_ms=${t1 - stopRequestedAt} ran_ms=${t1 - t0}"
                NativeBridge.STATUS_ERROR -> "RESULT runtime=$runtime n=$n error ran_ms=${t1 - t0}"
                else -> "RESULT runtime=$runtime n=$n sum=$r ms=${t1 - t0}"
            }
            publish(line)
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        }
        t.name = "nc-wasm-job"
        worker = t
        t.start()
    }

    private fun requestStop() {
        stopRequestedAt = SystemClock.elapsedRealtime()
        NativeBridge.nativeStop()
        if (worker?.isAlive != true) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        }
    }

    private fun goForeground() {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL, "Helper session", NotificationManager.IMPORTANCE_LOW)
        )
        val stopIntent = PendingIntent.getService(
            this, 0,
            Intent(this, HelperService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )
        val openIntent = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE
        )
        val notif = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle("Helping")
            .setContentText("Running a test job. Tap Stop to end it.")
            .setOngoing(true)
            .setContentIntent(openIntent)
            .addAction(Notification.Action.Builder(null, "Stop", stopIntent).build())
            .build()
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(NOTIF_ID, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(NOTIF_ID, notif)
        }
    }

    override fun onDestroy() {
        NativeBridge.nativeStop()
        super.onDestroy()
    }
}
