package io.fesen.bridge

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Activity
import android.app.PendingIntent
import android.content.Intent
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.VpnService
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.os.Looper
import android.os.Message
import android.os.Messenger
import android.os.ParcelFileDescriptor
import android.os.ResultReceiver
import android.widget.Toast
import org.json.JSONArray
import org.json.JSONObject
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress

class FesenVpn : VpnService() {
    companion object {
        const val IPC = "io.fesen.bridge.IPC"
        internal const val START = "io.fesen.bridge.START"
        internal const val ARM = "io.fesen.bridge.ARM"
        private const val STOP = "io.fesen.bridge.STOP"
        internal var current: FesenVpn? = null
            private set
        init { System.loadLibrary("fesen_lib") }
    }

    private external fun nativeStart(path: String): Long
    private external fun nativeRequest(engine: Long, request: String): String
    private external fun nativeRunning(engine: Long): Boolean
    private external fun nativeHalt(engine: Long)
    private external fun nativeDrop(engine: Long)

    private val display = Handler(Looper.getMainLooper())
    private lateinit var worker: HandlerThread
    private lateinit var ipc: Messenger
    private lateinit var background: Handler
    private val engineLock = Any()
    private val coreLifecycleLock = Any()
    private val ipcPool = java.util.concurrent.Executors.newFixedThreadPool(4)
    @Volatile private var engine = 0L
    @Volatile private var stopping = false
    private var starting = false
    private var running = false
    @Volatile private var tunnel = false
    private var foreground = false
    internal val engaged: Boolean get() = !stopping && tunnel
    internal val pending: Boolean get() = starting || running
    internal val shuttingDown: Boolean get() = stopping
    private val callbacks = java.util.concurrent.ConcurrentHashMap<IBinder, Messenger>()
    private val networks by lazy { getSystemService(ConnectivityManager::class.java) }
    @Volatile private var physical: Network? = null
    @Volatile private var cachedUnderlying: List<Pair<Network, LinkProperties>>? = null
    private val physicalChanges = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(net: Network) { physical = net; cachedUnderlying = null }
        override fun onLost(net: Network) { if (physical == net) physical = null; cachedUnderlying = null }
        override fun onLinkPropertiesChanged(net: Network, linkProperties: LinkProperties) { cachedUnderlying = null }
    }

    override fun onCreate() {
        super.onCreate()
        current = this
        worker = HandlerThread("fesen-ipc").also { it.start() }
        background = Handler(worker.looper)
        networks.registerBestMatchingNetworkCallback(
            NetworkRequest.Builder().addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET).build(), physicalChanges, display)
        ipc = Messenger(Handler(worker.looper) { request ->
            val body = request.data.getString("request")
            if (body == null) return@Handler true
            val replyTo = request.replyTo
            val reqId = request.arg1
            replyTo?.let { callbacks[it.binder] = it }
            runCatching {
                ipcPool.execute {
                    val reply = execute(body)
                    runCatching {
                        replyTo?.send(Message.obtain(null, 1, reqId, 0).apply {
                            data = Bundle().apply { putString("reply", reply) }
                        })
                    }
                }
            }
            true
        })
    }

    private fun execute(body: String): String {
        val currentEngine = synchronized(coreLifecycleLock) {
            synchronized(engineLock) {
                if (!stopping && engine == 0L) {
                    engine = nativeStart(filesDir.resolve("core").absolutePath)
                    if (stopping && engine != 0L) nativeHalt(engine)
                }
                engine
            }
        }
        val reply = if (stopping || currentEngine == 0L) """{"error":"not_connected"}""" else nativeRequest(currentEngine, body)
        val op = runCatching { JSONObject(body).optString("op") }.getOrNull()
        updateRunning(op == "login" || op == "start")
        return reply
    }

    private fun updateRunning(startFinished: Boolean = false) {
        val active = !stopping && engine != 0L && nativeRunning(engine)
        display.post {
            if (stopping) return@post
            if (!startFinished && running == active && (!foreground || starting || active)) return@post
            if (startFinished) starting = false
            running = active
            if (foreground && !starting && !active) standDown()
            FesenTile.refresh()
        }
    }

    internal fun refreshAccess() { background.post { updateRunning() } }

    override fun onBind(intent: Intent): IBinder? = if (intent.action == IPC) ipc.binder else super.onBind(intent)

    @Suppress("DEPRECATION")
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == STOP) { halt(); return START_NOT_STICKY }
        val ready = intent?.getParcelableExtra<ResultReceiver>("ready")
        if (intent?.action !in listOf(ARM, START) || prepare(this) != null) {
            ready?.send(Activity.RESULT_CANCELED, null); stopSelf(startId); return START_NOT_STICKY
        }
        if (stopping) {
            stopping = false
            tunnel = false
            running = false
        }
        val wasBusy = starting || running
        starting = true
        if (!foreground) {
            startForeground(27, notification())
            foreground = true
        }
        if (intent?.action == ARM) {
            ready?.send(Activity.RESULT_OK, null)
            // If the requesting Activity disappears before sending its IPC, don't strand a notification.
            display.postDelayed({ if (starting && !running && !stopping) { starting = false; standDown() } }, 30_000)
        } else if (!wasBusy) {
            FesenTile.refresh()
            background.post {
                execute("""{"op":"start"}""")
                display.post {
                    starting = false
                    if (!stopping && !running) {
                        Toast.makeText(this, R.string.fesen_connect_failed, Toast.LENGTH_SHORT).show()
                        halt()
                    }
                    FesenTile.refresh()
                }
            }
        } else starting = false
        return START_NOT_STICKY
    }

    private fun standDown() {
        foreground = false
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun notification(): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel("fesen", "VPN", NotificationManager.IMPORTANCE_LOW).apply {
            setSound(null, null)
            enableVibration(false)
            setShowBadge(false)
        })
        val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        val stop = PendingIntent.getService(this, 1, Intent(this, FesenVpn::class.java).setAction(STOP), flags)
        val open = packageManager.getLaunchIntentForPackage(packageName)?.let { PendingIntent.getActivity(this, 0, it, flags) }
        return Notification.Builder(this, "fesen").setSmallIcon(R.drawable.fesen_tile)
            .setContentTitle("${getString(R.string.fesen_tile_label)} VPN").setContentIntent(open).setOnlyAlertOnce(true).setOngoing(true)
            .setShowWhen(false).setCategory(Notification.CATEGORY_SERVICE)
            .addAction(Notification.Action.Builder(null, getString(R.string.fesen_disconnect), stop).build()).build()
    }

    override fun onRevoke() { display.post { halt() } }

    internal fun halt() {
        if (stopping) return
        stopping = true
        starting = false
        running = false
        tunnel = false
        FesenTile.refresh()
        val toDrop = synchronized(engineLock) {
            val e = engine
            engine = 0L
            e
        }
        if (toDrop != 0L) {
            nativeHalt(toDrop)
            background.post { synchronized(coreLifecycleLock) { nativeDrop(toDrop) } }
        }
        callbacks.values.forEach { runCatching { it.send(Message.obtain(null, 2)) } }
        callbacks.clear()
        standDown()
    }

    override fun onDestroy() {
        halt()
        networks.unregisterNetworkCallback(physicalChanges)
        cachedUnderlying = null
        if (current === this) current = null
        FesenTile.refresh(this)
        ipcPool.shutdown()
        background.post {
            runCatching { ipcPool.awaitTermination(2, java.util.concurrent.TimeUnit.SECONDS) }
            worker.quitSafely()
        }
        super.onDestroy()
    }

    @Suppress("unused")
    fun coreExpired() { display.post { halt() } }

    @Suppress("DEPRECATION")
    private fun underlying(): List<Pair<Network, LinkProperties>> {
        val active = networks.activeNetwork
        val cap = active?.let(networks::getNetworkCapabilities)
        val primary = physical ?: active?.takeIf { cap?.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN) == true }
        return networks.allNetworks.mapNotNull { net ->
            val capabilities = networks.getNetworkCapabilities(net) ?: return@mapNotNull null
            if (!capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)) return@mapNotNull null
            if (!capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
                !capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) &&
                !capabilities.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) return@mapNotNull null
            val props = networks.getLinkProperties(net) ?: return@mapNotNull null
            if (props.linkAddresses.none { it.address is Inet4Address }) return@mapNotNull null
            net to props
        }.sortedWith(compareBy<Pair<Network, LinkProperties>> { it.first != primary }.thenBy { it.first.networkHandle })
    }

    private fun addresses(props: LinkProperties?) = props?.linkAddresses.orEmpty()
        .map { it.address }.filterIsInstance<Inet4Address>().map { it.hostAddress }.sortedBy { it }

    private fun globalIpv6(props: LinkProperties?) = props?.linkAddresses.orEmpty()
        .map { it.address }.filterIsInstance<Inet6Address>()
        .filter { (it.address[0].toInt() and 0xe0) == 0x20 }
        .mapNotNull { it.hostAddress?.substringBefore('%') }.sorted()

    @Suppress("unused")
    fun deviceJson(): String = runCatching {
        val androidId = android.provider.Settings.Secure.getString(
            contentResolver,
            android.provider.Settings.Secure.ANDROID_ID
        ).orEmpty()
        val custom = runCatching {
            android.provider.Settings.Global.getString(
                contentResolver,
                android.provider.Settings.Global.DEVICE_NAME
            )
        }.getOrNull()?.trim().orEmpty()
        val model = android.os.Build.MODEL.orEmpty().trim()
        val brand = android.os.Build.MANUFACTURER.orEmpty().trim()
        val fallback = when {
            model.isEmpty() -> "Android"
            brand.isEmpty() || model.startsWith(brand, ignoreCase = true) -> model
            else -> "$brand $model"
        }
        val name = custom.ifEmpty { fallback }.take(64)
        JSONObject()
            .put("machine_id", androidId)
            .put("name", name)
            .toString()
    }.getOrElse { """{"machine_id":"","name":"Android"}""" }

    @Suppress("unused")
    fun networkJson(): String = runCatching {
        val list = underlying()
        cachedUnderlying = list
        val active = list.firstOrNull()
        val alternate = list.getOrNull(1)
        val routes = JSONArray()
        list.forEachIndexed { i, (_, props) ->
            props.routes.filter { it.destination.address is Inet4Address }.sortedBy { it.destination.toString() }.forEach { route ->
                if (route.destination.prefixLength != 0 || i == 0) routes.put(JSONObject()
                    .put("cidr", route.destination.toString()).put("index", i + 1))
            }
        }
        JSONObject()
            .put("marker", list.take(2).joinToString(";") { (net, props) ->
                "${net.networkHandle}/${props.interfaceName}/${addresses(props)}/${globalIpv6(props)}/${props.routes.filter { it.isDefaultRoute }}"
            })
            .put("handle", active?.first?.networkHandle?.toString() ?: "")
            .put("addresses", JSONArray(addresses(active?.second)))
            .put("ipv6", JSONArray(globalIpv6(active?.second)))
            .put("routes", routes)
            .put("dns", JSONArray(active?.second?.dnsServers.orEmpty().filterIsInstance<Inet4Address>().map { it.hostAddress }))
            .put("alternate", alternate?.let { (net, props) -> JSONObject()
                .put("handle", net.networkHandle.toString()).put("addresses", JSONArray(addresses(props))) })
            .toString()
    }.getOrElse { """{"marker":"","handle":"","addresses":[],"routes":[],"dns":[]}""" }

    @Suppress("unused")
    fun protectSocket(fd: Int, expected: String, target: String, alternate: String): Boolean = runCatching {
        val cached = cachedUnderlying
        val list = if (cached != null && cached.firstOrNull()?.first?.networkHandle?.toString() == expected) {
            cached
        } else {
            underlying().also { cachedUnderlying = it }
        }
        val primary = list.firstOrNull()?.first ?: return false
        if (primary.networkHandle.toString() != expected) return false
        val network = when {
            alternate.isNotEmpty() -> list.getOrNull(1)?.first?.takeIf { it.networkHandle.toString() == alternate } ?: return false
            target.isNotEmpty() -> {
                val ip = InetAddress.getByName(target)
                list.maxByOrNull { (_, props) -> props.routes.filter { it.matches(ip) }
                    .maxOfOrNull { it.destination.prefixLength } ?: -1 }?.first ?: primary
            }
            else -> primary
        }
        if (!protect(fd)) return false
        ParcelFileDescriptor.fromFd(fd).use { network.bindSocket(it.fileDescriptor) }
        true
    }.getOrDefault(false)

    @Suppress("unused")
    fun configureTun(json: String): Int = runCatching {
        val config = JSONObject(json)
        if (config.isNull("address") || stopping) {
            tunnel = false; refreshAccess(); display.post { FesenTile.refresh() }; return -1
        }
        check(prepare(this) == null)
        val builder = Builder().setSession("Fesen").setMtu(1500).setBlocking(false)
            .addAddress(config.getString("address"), 32)
        packageManager.getLaunchIntentForPackage(packageName)?.let {
            builder.setConfigureIntent(PendingIntent.getActivity(this, 0, it, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))
        }
        val routes = config.getJSONArray("routes")
        for (index in 0 until routes.length()) {
            val cidr = routes.getString(index).split("/")
            val prefix = cidr[1].toInt()
            if (prefix !in 1..32) return -1
            builder.addRoute(cidr[0], prefix)
        }
        if (!config.isNull("dns")) {
            builder.addDnsServer(config.getString("dns")).addSearchDomain("fesen")
        }
        builder.allowFamily(android.system.OsConstants.AF_INET6)
        builder.setUnderlyingNetworks(underlying().take(1).map { it.first }.toTypedArray())
        val fd = checkNotNull(builder.establish()).detachFd()
        tunnel = true
        display.post { FesenTile.refresh() }
        fd
    }.getOrElse {
        tunnel = false
        display.post { Toast.makeText(this, R.string.fesen_connect_failed, Toast.LENGTH_SHORT).show(); halt() }
        -1
    }
}
