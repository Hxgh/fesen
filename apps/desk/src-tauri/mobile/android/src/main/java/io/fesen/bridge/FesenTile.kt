package io.fesen.bridge

import android.app.PendingIntent
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

class FesenTile : TileService() {
    companion object {
        private var listening: FesenTile? = null
        internal fun refresh(context: Context? = FesenVpn.current) {
            listening?.render()
            val ctx = context?.applicationContext ?: return
            runCatching { requestListeningState(ctx, ComponentName(ctx, FesenTile::class.java)) }
        }
        init { System.loadLibrary("fesen_lib") }
    }

    private external fun nativeCanResume(path: String): Boolean

    override fun onStartListening() {
        listening = this
        render()
        FesenVpn.current?.refreshAccess()
    }

    override fun onStopListening() {
        if (listening === this) listening = null
    }

    override fun onDestroy() {
        onStopListening()
        super.onDestroy()
    }

    private fun render() {
        val service = FesenVpn.current
        val engaged = service?.engaged == true
        val pending = !engaged && service?.pending == true && service.shuttingDown == false
        qsTile?.apply {
            state = if (engaged || pending) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
            subtitle = getString(
                when {
                    engaged -> R.string.fesen_tile_on
                    pending -> R.string.fesen_tile_connecting
                    else -> R.string.fesen_tile_off
                }
            )
            updateTile()
        }
    }

    override fun onClick() {
        if (isLocked) unlockAndRun { toggle() } else toggle()
    }

    private fun toggle() {
        val service = FesenVpn.current
        if (service != null && !service.shuttingDown && (service.engaged || service.pending)) {
            service.halt()
            return
        }
        if (VpnService.prepare(this) != null || !nativeCanResume(filesDir.resolve("core").absolutePath)) {
            openApp()
            return
        }
        val intent = Intent(this, FesenVpn::class.java).setAction(FesenVpn.START)
        if (runCatching { startForegroundService(intent) }.isFailure) openApp()
    }

    @Suppress("DEPRECATION")
    private fun openApp() {
        val intent = packageManager.getLaunchIntentForPackage(packageName) ?: return
        if (Build.VERSION.SDK_INT >= 34) {
            startActivityAndCollapse(PendingIntent.getActivity(this, 0, intent, PendingIntent.FLAG_IMMUTABLE))
        } else {
            startActivityAndCollapse(intent)
        }
    }
}
