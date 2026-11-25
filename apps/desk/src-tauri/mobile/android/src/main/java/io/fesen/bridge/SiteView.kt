package io.fesen.bridge

import android.annotation.SuppressLint
import android.app.Activity
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.Uri
import android.view.Gravity
import android.view.ViewGroup
import android.webkit.*
import android.widget.FrameLayout
import app.tauri.plugin.Invoke
import org.json.JSONArray
import java.io.ByteArrayInputStream
import java.net.InetAddress

/** The remote page is a separate, bridge-free child within the trusted dialog's content rectangle. */
internal class SiteView(private val activity: Activity, private val main: WebView) {
    private val root get() = activity.findViewById<FrameLayout>(android.R.id.content)
    private var page: WebView? = null
    private var attempt = 0L
    private var error: String? = null
    private var routeOk: () -> Boolean = { false }

    fun command(invoke: Invoke) {
        runCatching {
            val args = invoke.getArgs()
            val id = args.getLong("attempt")
            val op = args.getString("op")
            if (op == "open") {
                close()
                attempt = id
                open(Uri.parse(args.getString("url")), args.getBoolean("group"), args.getJSONArray("bounds"))
            } else if (id != attempt) {
                invoke.resolveObject("closed"); return
            } else when (op) {
                "move" -> place(args.getJSONArray("bounds"))
                "close" -> close()
                "inspect" -> {
                    val failed = error ?: if (!routeOk()) "route_missing" else null
                    val web = page
                    if (failed != null || web == null) invoke.resolveObject(failed ?: "closed")
                    else web.evaluateJavascript(args.getString("script")) { value ->
                        invoke.resolveObject(runCatching { JSONArray("[$value]").getString(0) }.getOrDefault("loading"))
                    }
                    return
                }
                else -> error("invalid operation")
            }
            invoke.resolveObject(error ?: "ok")
        }.onFailure { close(); invoke.reject("browser_failed") }
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun open(uri: Uri, group: Boolean, bounds: JSONArray) {
        require(uri.scheme == "http" && uri.userInfo == null && uri.path == "/" && uri.query == null &&
            uri.fragment == null && uri.host?.matches(Regex("""\d{1,3}(\.\d{1,3}){3}""")) == true && uri.port in 1..65535)
        val ip = InetAddress.getByName(uri.host)
        val cm = activity.getSystemService(ConnectivityManager::class.java)
        routeOk = { !group || cm.activeNetwork?.let { net ->
            val cap = cm.getNetworkCapabilities(net)
            cap?.hasTransport(NetworkCapabilities.TRANSPORT_VPN) == true &&
                cap.ownerUid == android.os.Process.myUid() &&
                cm.getLinkProperties(net)?.routes?.any { it.destination.prefixLength > 0 && it.matches(ip) } == true
        } == true }
        if (!routeOk()) { error = "route_missing"; return }
        // Uri normalizes the scheme but omits :80 in some callbacks.
        fun allowed(u: Uri) = u.scheme == "http" && u.userInfo == null && u.host == uri.host &&
            (if (u.port == -1) 80 else u.port) == uri.port
        val web = WebView(activity)
        page = web
        with(web.settings) {
            javaScriptEnabled = true
            domStorageEnabled = true
            allowFileAccess = false
            allowContentAccess = false
            setGeolocationEnabled(false)
            setSupportMultipleWindows(false)
            javaScriptCanOpenWindowsAutomatically = false
            cacheMode = WebSettings.LOAD_NO_CACHE
            mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
        }
        CookieManager.getInstance().setAcceptThirdPartyCookies(web, false)
        web.setDownloadListener { _,_,_,_,_ -> }
        web.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(v: WebView, r: WebResourceRequest) = !allowed(r.url) || !routeOk()
            override fun shouldInterceptRequest(v: WebView, r: WebResourceRequest): WebResourceResponse? =
                if (allowed(r.url) && routeOk()) null else WebResourceResponse("text/plain", "UTF-8", 403, "Blocked", emptyMap(), ByteArrayInputStream(byteArrayOf()))
            override fun onReceivedError(v: WebView, r: WebResourceRequest, e: WebResourceError) {
                if (v === page && r.isForMainFrame) error = when {
                    e.description.contains("ERR_EMPTY_RESPONSE") -> "page_empty"
                    e.errorCode == ERROR_TIMEOUT -> "target_timeout"
                    else -> "target_unavailable"
                }
            }
            override fun onReceivedHttpError(v: WebView, r: WebResourceRequest, e: WebResourceResponse) {
                if (v === page && r.isForMainFrame) error = "HTTP ${e.statusCode}"
            }
            override fun onRenderProcessGone(v: WebView, detail: RenderProcessGoneDetail): Boolean {
                if (v === page) { close(); error = "browser_failed" }
                return true
            }
        }
        root.addView(web)
        place(bounds)
        web.loadUrl(uri.toString())
    }

    private fun place(b: JSONArray) {
        val host = IntArray(2); val outer = IntArray(2)
        main.getLocationInWindow(host); root.getLocationInWindow(outer)
        page?.layoutParams = FrameLayout.LayoutParams(b.getInt(2), b.getInt(3), Gravity.TOP or Gravity.LEFT).apply {
            leftMargin = host[0] - outer[0] - root.paddingLeft + b.getInt(0)
            topMargin = host[1] - outer[1] - root.paddingTop + b.getInt(1)
        }
    }

    fun close() {
        val web = page
        page = null; error = null; routeOk = { false }
        if (web != null) {
            (web.parent as? ViewGroup)?.removeView(web)
            web.stopLoading(); web.destroy()
        }
    }
}
