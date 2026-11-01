package lk.networkconnector.spike

import android.app.Activity
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView

/** Two buttons and a status line. Stop is always visible. */
class MainActivity : Activity() {

    private lateinit var statusView: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 33) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        statusView = TextView(this).apply { textSize = 16f; setTextIsSelectable(true) }
        val start = Button(this).apply {
            text = "Start test job"
            setOnClickListener {
                startForegroundService(
                    Intent(this@MainActivity, HelperService::class.java)
                        .setAction(HelperService.ACTION_START)
                        .putExtra(HelperService.EXTRA_N, 300_000_000L)
                )
            }
        }
        val stop = Button(this).apply {
            text = "Stop"
            setOnClickListener {
                startService(
                    Intent(this@MainActivity, HelperService::class.java)
                        .setAction(HelperService.ACTION_STOP)
                )
            }
        }
        setContentView(LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 96, 48, 48)
            addView(stop)
            addView(start)
            addView(statusView)
        })
    }

    override fun onResume() {
        super.onResume()
        statusView.text = HelperService.status
        HelperService.listener = { s -> runOnUiThread { statusView.text = s } }
    }

    override fun onPause() {
        HelperService.listener = null
        super.onPause()
    }
}
