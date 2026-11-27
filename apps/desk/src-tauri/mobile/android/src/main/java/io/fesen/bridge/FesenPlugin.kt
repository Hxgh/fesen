package io.fesen.bridge

import android.app.Activity
import android.app.StatusBarManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.res.Configuration
import android.graphics.drawable.Icon
import android.net.VpnService
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Message
import android.os.Messenger
import android.os.ResultReceiver
import android.view.View
import android.view.WindowManager
import android.webkit.WebView
import androidx.activity.result.ActivityResult
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@TauriPlugin
class FesenPlugin(private val activity: Activity) : Plugin(activity) {
    private val main = Handler(Looper.getMainLooper())
    private var service: Messenger? = null
    private var preview: SiteView? = null
    private var bound = false
    private var serial = 0
    private val queued = ArrayList<Invoke>()
    private val awaiting = HashMap<Int, Invoke>()
    private val responses = Messenger(Handler(Looper.getMainLooper()) { message ->
        if (message.what == 2) {
            release()
            return@Handler true
        }
        val invoke = awaiting.remove(message.arg1)
        val result = message.data.getString("reply")
        if (invoke != null) {
            if (result == null) invoke.reject("not_connected")
            else try { invoke.resolve(JSObject(result)) } catch (_: Exception) { invoke.reject("internal") }
        }
        true
    })

    override fun load(webView: WebView) {
        activity.runOnUiThread {
            webView.overScrollMode = View.OVER_SCROLL_NEVER
            preview = SiteView(activity, webView)
            val root = activity.findViewById<View>(android.R.id.content)
            activity.window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
            ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
                val safe = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
                val keyboard = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom
                view.setPadding(safe.left, safe.top, safe.right, maxOf(safe.bottom, keyboard))
                WindowInsetsCompat.CONSUMED
            }
            onConfigurationChanged(activity.resources.configuration)
            ViewCompat.requestApplyInsets(root)
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        val dark = newConfig.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        val root = activity.findViewById<View>(android.R.id.content)
        root.setBackgroundColor(if (dark) 0xff0c0d0c.toInt() else 0xfff7f5f0.toInt())
        WindowCompat.getInsetsController(activity.window, activity.window.decorView).apply {
            isAppearanceLightStatusBars = !dark
            isAppearanceLightNavigationBars = !dark
        }
    }

    @Command
    fun siteView(invoke: Invoke) { main.post { preview?.command(invoke) ?: invoke.reject("browser_failed") } }

    @Command
    fun addTile(invoke: Invoke) {
        main.post {
            val sbm = activity.getSystemService(StatusBarManager::class.java)
            if (sbm == null) {
                invoke.reject("internal")
                return@post
            }
            val component = ComponentName(activity, FesenTile::class.java)
            val label = activity.getString(R.string.fesen_tile_label)
            val icon = Icon.createWithResource(activity, R.drawable.fesen_tile)
            runCatching {
                sbm.requestAddTileService(component, label, icon, activity.mainExecutor) { result ->
                    val status = when (result) {
                        StatusBarManager.TILE_ADD_REQUEST_RESULT_TILE_ADDED -> "added"
                        StatusBarManager.TILE_ADD_REQUEST_RESULT_TILE_ALREADY_ADDED -> "already_added"
                        else -> "dismissed"
                    }
                    invoke.resolve(JSObject().put("status", status))
                }
            }.onFailure { invoke.reject("internal") }
        }
    }

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            service = Messenger(binder)
            queued.toList().also { queued.clear() }.forEach(::send)
        }
        override fun onServiceDisconnected(name: ComponentName) = release()
        override fun onBindingDied(name: ComponentName) = release()
        override fun onNullBinding(name: ComponentName) = release()
    }

    private fun release() {
        service = null
        if (bound) {
            bound = false
            runCatching { activity.unbindService(connection) }
        }
        queued.forEach { it.reject("not_connected") }
        queued.clear()
        awaiting.values.forEach { it.reject("not_connected") }
        awaiting.clear()
    }

    private fun send(invoke: Invoke) {
        val target = service ?: return invoke.reject("not_connected")
        val id = ++serial
        val request = Message.obtain(null, 1, id, 0)
        request.replyTo = responses
        request.data = Bundle().apply { putString("request", invoke.getArgs().getJSONObject("request").toString()) }
        awaiting[id] = invoke
        try { target.send(request) } catch (_: Exception) { release(); return }
        main.postDelayed({ awaiting.remove(id)?.reject("not_connected") }, 28_000)
    }

    private fun arm(invoke: Invoke) {
        // Dispatch login/start only once the explicitly requested foreground service is ready.
        val ready = object : ResultReceiver(main) {
            override fun onReceiveResult(resultCode: Int, resultData: Bundle?) {
                if (resultCode == Activity.RESULT_OK) connect(invoke, true) else invoke.reject("vpn_permission")
            }
        }
        runCatching { activity.startForegroundService(Intent(activity, FesenVpn::class.java)
            .setAction(FesenVpn.ARM).putExtra("ready", ready)) }.onFailure { invoke.reject("vpn_permission") }
    }

    private fun connect(invoke: Invoke, create: Boolean) {
        if (service != null) return send(invoke)
        queued.add(invoke)
        val intent = Intent(activity, FesenVpn::class.java)
        try {
            if (!bound) {
                bound = activity.bindService(intent.setAction(FesenVpn.IPC), connection,
                    if (create) Context.BIND_AUTO_CREATE else 0)
            }
            if (!bound) release()
        } catch (_: Exception) { release() }
        main.postDelayed({
            if (service == null && queued.contains(invoke)) release()
        }, if (create) 8_000 else 1_200)
    }

    @Command
    fun request(invoke: Invoke) {
        main.post {
            val args = invoke.getArgs()
            val op = args.optJSONObject("request")?.optString("op")
            if (op != "start" && op != "login") {
                connect(invoke, args.optBoolean("launch"))
                return@post
            }
            val permission = VpnService.prepare(activity)
            if (permission == null) arm(invoke)
            else startActivityForResult(invoke, permission, "vpnResult")
        }
    }

    @ActivityCallback
    private fun vpnResult(invoke: Invoke, result: ActivityResult) {
        if (result.resultCode == Activity.RESULT_OK) arm(invoke) else invoke.reject("vpn_permission")
    }

    @Suppress("DEPRECATION", "OVERRIDE_DEPRECATION")
    override fun onDestroy() { preview?.close(); preview = null; release() }
}
