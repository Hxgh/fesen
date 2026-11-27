import AppKit
import CoreGraphics
import Darwin
import Foundation
import NetworkExtension

@main private struct Tray {
    static func main() {
        let args = CommandLine.arguments
        let executable = URL(fileURLWithPath: args[0]).resolvingSymlinksInPath()
        let menuApp = executable
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let bundle = menuApp
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        guard (NSDictionary(contentsOf: bundle.appendingPathComponent("Contents/Info.plist"))?["CFBundleIdentifier"] as? String) == "io.fesen.client" else { return }
        if args.count == 3, args[1] == "--spawn", ["--local", "--extension"].contains(args[2]) {
            // Relaunch via LaunchServices from a non-AppKit process so the GUI never owns parentASN.
            let config = NSWorkspace.OpenConfiguration()
            config.createsNewApplicationInstance = true
            config.activates = false
            config.arguments = [args[2]]
            let done = DispatchSemaphore(value: 0)
            NSWorkspace.shared.openApplication(at: menuApp, configuration: config) { _, _ in
                done.signal()
            }
            _ = done.wait(timeout: .now() + 5)
            return
        }
        guard args.count == 2, ["--local", "--extension"].contains(args[1]) else { return }
        let cache = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Caches/io.fesen.client")
        try? FileManager.default.createDirectory(at: cache, withIntermediateDirectories: true)
        let fd = open(cache.appendingPathComponent("menu.lock").path, O_CREAT | O_RDWR | O_NOFOLLOW, 0o600)
        guard fd >= 0 else { return }
        defer { close(fd) }
        var locked = false
        for _ in 0..<10 {
            if flock(fd, LOCK_EX | LOCK_NB) == 0 {
                locked = true
                break
            }
            usleep(50_000)
        }
        guard locked else { return }
        MainActor.assumeIsolated {
            Menu(bundle: bundle, usesExtension: args[1] == "--extension").run()
        }
    }
}

private enum MarkState: Equatable {
    case idle
    case connecting
    case connected
}

private struct IpcAddress: Decodable {
    let port: UInt16
    let token: [UInt8]
}

private enum LocalCoreIPC {
    private static var ipcURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/io.fesen.client/publisher/ipc.json")
    }

    static func queryState() -> MarkState {
        guard let reply = send(json: "{\"op\":\"status\",\"ui\":false}"),
              let data = reply["data"] as? [String: Any]
        else { return .idle }
        if (data["online"] as? Bool) == true { return .connected }
        if (data["running"] as? Bool) == true { return .connecting }
        return .idle
    }

    @discardableResult
    static func sendStop() -> Bool {
        send(json: "{\"op\":\"stop\"}", timeoutMs: 800) != nil
    }

    @discardableResult
    static func sendQuit() -> Bool {
        send(json: "{\"op\":\"quit\"}", timeoutMs: 600) != nil
    }

    static func sendStart() -> (state: MarkState, signedIn: Bool)? {
        guard let reply = send(json: "{\"op\":\"start\"}", timeoutMs: 3000) else { return nil }
        guard let data = reply["data"] as? [String: Any] else {
            return (.idle, false)
        }
        let signedIn = data["member"] is String
        if (data["online"] as? Bool) == true { return (.connected, signedIn) }
        if (data["running"] as? Bool) == true { return (.connecting, signedIn) }
        return (.idle, signedIn)
    }

    private static func send(json: String, timeoutMs: Int = 350) -> [String: Any]? {
        guard let raw = try? Data(contentsOf: ipcURL),
              let addr = try? JSONDecoder().decode(IpcAddress.self, from: raw),
              addr.token.count == 32, addr.port > 0
        else { return nil }

        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        defer { close(fd) }

        var tv = timeval(tv_sec: timeoutMs / 1000, tv_usec: __darwin_suseconds_t((timeoutMs % 1000) * 1000))
        _ = setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        _ = setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var noSigPipe: Int32 = 1
        _ = setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, socklen_t(MemoryLayout<Int32>.size))

        var sin = sockaddr_in()
        sin.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        sin.sin_family = sa_family_t(AF_INET)
        sin.sin_port = addr.port.bigEndian
        sin.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))

        let ok = withUnsafePointer(to: &sin) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) == 0
            }
        }
        guard ok else { return nil }

        let payload = Data(json.utf8)
        var packet = Data(capacity: 32 + 4 + payload.count)
        packet.append(contentsOf: addr.token)
        var lenBE = UInt32(payload.count).bigEndian
        withUnsafeBytes(of: &lenBE) { packet.append(contentsOf: $0) }
        packet.append(payload)

        guard writeAll(fd, packet) else { return nil }
        guard let header = readExact(fd, count: 4) else { return nil }
        let replyLen = header.withUnsafeBytes { $0.load(as: UInt32.self).bigEndian }
        guard replyLen > 0, replyLen <= 1_048_576,
              let body = readExact(fd, count: Int(replyLen)),
              let obj = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        else { return nil }
        return obj
    }

    private static func writeAll(_ fd: Int32, _ data: Data) -> Bool {
        data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return false }
            var offset = 0
            while offset < data.count {
                let n = Darwin.write(fd, base.advanced(by: offset), data.count - offset)
                if n <= 0 { return false }
                offset += n
            }
            return true
        }
    }

    private static func readExact(_ fd: Int32, count: Int) -> Data? {
        var out = Data(count: count)
        let ok = out.withUnsafeMutableBytes { raw -> Bool in
            guard let base = raw.baseAddress else { return false }
            var offset = 0
            while offset < count {
                let n = Darwin.read(fd, base.advanced(by: offset), count - offset)
                if n <= 0 { return false }
                offset += n
            }
            return true
        }
        return ok ? out : nil
    }
}

private let trigramPaths: [[UInt8]] = [
    [0b001, 0b101],
    [0b010, 0b110],
    [0b001, 0b011, 0b111, 0b110],
    [0b010, 0b011, 0b111, 0b101],
]

@MainActor private final class Menu: Accessory, NSMenuDelegate {
    private let usesExtension: Bool
    private let zh = Locale.preferredLanguages.first?.hasPrefix("zh") == true
    private var item: NSStatusItem?
    private var toggleItem: NSMenuItem?
    private var tunnel: NETunnelProviderManager?
    private var markState: MarkState = .idle
    private var currentMask: UInt8 = 0b000
    private var prevMask: UInt8 = 0xff
    private var queue: [UInt8] = []
    private var animTimer: Timer?
    private var pollTicks = 0
    private var publisherRunning = false
    private var publisherProcess: Process?
    private var allowTerminate = false

    init(bundle: URL, usesExtension: Bool) {
        self.usesExtension = usesExtension
        super.init(bundle: bundle)
    }

    // Bitmask-driven Bagua @2x CGImage template (36x36px -> 18x18pt).
    // Bit 2 = top yao, Bit 1 = mid yao, Bit 0 = bot yao.
    // hotBit highlights only the actively changing yao during connecting (0 = uniform alpha).
    private func trigramImage(_ mask: UInt8, alpha: CGFloat, hotBit: UInt8 = 0) -> NSImage {
        let px = 36
        let colorSpace = CGColorSpaceCreateDeviceRGB()
        let bitmapInfo = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)
        guard let ctx = CGContext(
            data: nil,
            width: px,
            height: px,
            bitsPerComponent: 8,
            bytesPerRow: px * 4,
            space: colorSpace,
            bitmapInfo: bitmapInfo.rawValue
        ) else {
            return NSImage(size: NSSize(width: 18, height: 18))
        }
        let rows: [(bit: UInt8, y: CGFloat)] = [(1, 6), (2, 16), (4, 26)]
        for row in rows {
            let rowAlpha: CGFloat = hotBit == 0 ? alpha : ((hotBit & row.bit) != 0 ? 1.0 : 0.36)
            ctx.setFillColor(red: 0, green: 0, blue: 0, alpha: rowAlpha)
            if (mask & row.bit) != 0 {
                ctx.fill(CGRect(x: 5, y: row.y, width: 26, height: 4))
            } else {
                ctx.fill(CGRect(x: 5, y: row.y, width: 10, height: 4))
                ctx.fill(CGRect(x: 21, y: row.y, width: 10, height: 4))
            }
        }
        guard let cg = ctx.makeImage() else {
            return NSImage(size: NSSize(width: 18, height: 18))
        }
        let image = NSImage(cgImage: cg, size: NSSize(width: 18, height: 18))
        image.isTemplate = true
        image.accessibilityDescription = "Fesen"
        return image
    }

    private func renderMask(_ mask: UInt8, alpha: CGFloat, highlightChanged: Bool = false) {
        let diff: UInt8 = highlightChanged ? (currentMask ^ mask) : 0
        prevMask = currentMask
        currentMask = mask
        item?.button?.image = trigramImage(mask, alpha: alpha, hotBit: diff)
    }

    private func nextWalkMask() -> UInt8 {
        let candidates = [UInt8(1), 2, 4]
            .map { currentMask ^ $0 }
            .filter { $0 != 0b100 && $0 != 0b000 && $0 != prevMask }
        return candidates.randomElement() ?? 0b010
    }

    private func setMark(_ state: MarkState) {
        let changed = state != markState
        markState = state
        toggleItem?.title = switch state {
        case .connected: zh ? "断开连接" : "Disconnect"
        case .connecting: zh ? "取消连接" : "Cancel connecting"
        case .idle: zh ? "连接" : "Connect"
        }
        toggleItem?.isEnabled = true
        if state == .connecting {
            if animTimer == nil {
                queue = trigramPaths.randomElement() ?? [0b001, 0b101]
                pollTicks = 0
                let first = queue.isEmpty ? nextWalkMask() : queue.removeFirst()
                renderMask(first, alpha: 0.9, highlightChanged: true)
                let timer = Timer(timeInterval: 0.30, repeats: true) { [weak self] _ in
                    Task { @MainActor in
                        guard let self, self.markState == .connecting else { return }
                        let next = self.queue.isEmpty ? self.nextWalkMask() : self.queue.removeFirst()
                        self.renderMask(next, alpha: 0.9, highlightChanged: true)
                        self.pollTicks += 1
                        if !self.usesExtension && self.pollTicks % 3 == 0 {
                            self.syncStatus()
                        }
                    }
                }
                RunLoop.main.add(timer, forMode: .common)
                animTimer = timer
            }
        } else {
            animTimer?.invalidate()
            animTimer = nil
            queue.removeAll()
            if changed || item?.button?.image == nil {
                if state == .connected {
                    // Life Gate ☶ (Gen): solid top, full ink.
                    renderMask(0b100, alpha: 1.0)
                } else {
                    // Death Gate ☷ (Kun): all broken, soft ink.
                    renderMask(0b000, alpha: 0.50)
                }
            }
        }
    }

    private func syncStatus() {
        if usesExtension {
            let status = tunnel?.connection.status ?? .invalid
            switch status {
            case .connected:
                setMark(.connected)
            case .connecting, .reasserting:
                setMark(.connecting)
            default:
                setMark(.idle)
            }
        } else {
            DispatchQueue.global(qos: .utility).async { [weak self] in
                let state = LocalCoreIPC.queryState()
                DispatchQueue.main.async {
                    self?.setMark(state)
                }
            }
        }
    }

    private func showStatusItem() {
        guard item == nil else { return }
        let status = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        item = status
        renderMask(0b000, alpha: 0.50)
        let menu = NSMenu()
        menu.delegate = self
        menu.autoenablesItems = false
        menu.addItem(withTitle: zh ? "打开飞弦" : "Open Fesen", action: #selector(openApp), keyEquivalent: "").target = self
        toggleItem = menu.addItem(withTitle: zh ? "连接" : "Connect", action: #selector(toggleConnection), keyEquivalent: "")
        toggleItem?.target = self
        toggleItem?.isEnabled = true
        menu.addItem(.separator())
        menu.addItem(withTitle: zh ? "退出飞弦" : "Quit Fesen", action: #selector(quitApp), keyEquivalent: "").target = self
        status.menu = menu
        if markState == .connected {
            renderMask(0b100, alpha: 1.0)
        } else if markState == .connecting {
            renderMask(currentMask, alpha: 0.9, highlightChanged: true)
        }
    }

    private func hideStatusItem() {
        if let status = item {
            NSStatusBar.system.removeStatusItem(status)
        }
        item = nil
        toggleItem = nil
    }

    @objc func applicationDidFinishLaunching(_ notification: Notification) {
        ProcessInfo.processInfo.disableAutomaticTermination("Fesen background service")
        ProcessInfo.processInfo.disableSuddenTermination()
        DistributedNotificationCenter.default().addObserver(self, selector: #selector(preferenceChanged),
            name: MenuBarPreference.changed, object: bundle.path, suspensionBehavior: .deliverImmediately)
        DistributedNotificationCenter.default().addObserver(self, selector: #selector(statusNotified),
            name: MenuBarPreference.statusChanged, object: bundle.path, suspensionBehavior: .deliverImmediately)
        DistributedNotificationCenter.default().addObserver(self, selector: #selector(ensurePublisherRequested),
            name: MenuBarPreference.ensurePublisher, object: bundle.path, suspensionBehavior: .deliverImmediately)
        NSWorkspace.shared.notificationCenter.addObserver(self, selector: #selector(workspaceChanged),
            name: NSWorkspace.didLaunchApplicationNotification, object: nil)
        NSWorkspace.shared.notificationCenter.addObserver(self, selector: #selector(workspaceChanged),
            name: NSWorkspace.didTerminateApplicationNotification, object: nil)
        NSWorkspace.shared.notificationCenter.addObserver(self, selector: #selector(systemWillPowerOff),
            name: NSWorkspace.willPowerOffNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(vpnStatusChanged),
            name: .NEVPNStatusDidChange, object: nil)
        let enabled = MenuBarPreference.local.refresh()
        if usesExtension && !enabled { quitMenu(); return }
        if enabled { showStatusItem() }
        if usesExtension { Task { await refreshTunnel() } }
        else {
            ensurePublisherRunning()
            syncStatus()
        }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if allowTerminate || (!MenuBarPreference.local.refresh() && (usesExtension || !publisherRunning)) {
            return .terminateNow
        }
        return .terminateCancel
    }

    @objc private func systemWillPowerOff(_ notification: Notification) {
        allowTerminate = true
    }

    func applicationWillTerminate(_ notification: Notification) {
        if !usesExtension {
            if publisherProcess?.isRunning == true {
                _ = LocalCoreIPC.sendQuit()
                publisherProcess?.terminate()
                publisherProcess = nil
            }
        }
    }

    // Zero steady-state polling: refreshes on OS notifications, menu open, and briefly while connecting.
    func menuNeedsUpdate(_ menu: NSMenu) {
        if usesExtension && tunnel == nil { Task { await refreshTunnel() } }
        syncStatus()
    }

    private func refreshTunnel() async {
        tunnel = (try? await NETunnelProviderManager.loadAllFromPreferences())?.first {
            ($0.protocolConfiguration as? NETunnelProviderProtocol)?.providerBundleIdentifier == "io.fesen.client.tunnel"
        }
        syncStatus()
    }

    @objc private func workspaceChanged(_ notification: Notification) {
        guard !usesExtension else { return }
        syncStatus()
    }

    @objc private func vpnStatusChanged(_ notification: Notification) {
        guard usesExtension else { return }
        syncStatus()
    }

    @objc private func toggleConnection() {
        if markState == .idle {
            connect()
        } else {
            disconnect()
        }
    }

    private func ensurePublisherRunning() {
        guard !usesExtension else { return }
        if publisherProcess?.isRunning == true {
            publisherRunning = true
            return
        }
        let executable = bundle.appendingPathComponent("Contents/MacOS/fesen")
        let process = Process()
        process.executableURL = executable
        process.arguments = ["--publisher"]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        publisherRunning = true
        process.terminationHandler = { [weak self] _ in
            DispatchQueue.main.async {
                guard let self else { return }
                self.publisherProcess = nil
                self.publisherRunning = false
                self.setMark(.idle)
                if !MenuBarPreference.local.refresh() {
                    self.quitMenu()
                }
            }
        }
        if (try? process.run()) != nil {
            publisherProcess = process
        } else {
            publisherRunning = false
        }
    }

    @objc private func ensurePublisherRequested(_ notification: Notification) {
        ensurePublisherRunning()
    }

    private func launchBackgroundPublisher(completion: @escaping @Sendable (Bool) -> Void) {
        ensurePublisherRunning()
        completion(true)
    }

    private func connect() {
        setMark(.connecting)
        if usesExtension {
            Task { @MainActor in
                if tunnel == nil { await refreshTunnel() }
                guard let manager = tunnel else {
                    setMark(.idle)
                    openApp()
                    return
                }
                do {
                    try manager.connection.startVPNTunnel()
                } catch {
                    setMark(.idle)
                    openApp()
                }
            }
            return
        }
        DispatchQueue.global(qos: .utility).async { [weak self] in
            if let result = LocalCoreIPC.sendStart() {
                DispatchQueue.main.async {
                    guard let self else { return }
                    self.setMark(result.state)
                    if !result.signedIn { self.openApp() }
                }
                return
            }
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.launchBackgroundPublisher { [weak self] ok in
                    guard ok else {
                        DispatchQueue.main.async {
                            self?.setMark(.idle)
                            self?.openApp()
                        }
                        return
                    }
                    DispatchQueue.global(qos: .utility).async {
                        var result: (state: MarkState, signedIn: Bool)?
                        for _ in 0..<25 {
                            if let r = LocalCoreIPC.sendStart() {
                                result = r
                                break
                            }
                            Thread.sleep(forTimeInterval: 0.1)
                        }
                        DispatchQueue.main.async {
                            guard let self else { return }
                            if let result {
                                self.setMark(result.state)
                                if !result.signedIn { self.openApp() }
                            } else {
                                self.setMark(.idle)
                                self.openApp()
                            }
                        }
                    }
                }
            }
        }
    }

    private func disconnect() {
        if usesExtension {
            tunnel?.connection.stopVPNTunnel()
        } else {
            DispatchQueue.global(qos: .utility).async { [weak self] in
                let stopped = LocalCoreIPC.sendStop()
                DispatchQueue.main.async {
                    if !stopped {
                        _ = self?.instance(.accessory)?.terminate()
                    }
                    self?.setMark(.idle)
                }
            }
        }
    }

    @objc private func quitApp() {
        allowTerminate = true
        animTimer?.invalidate()
        animTimer = nil
        _ = instance(.regular)?.terminate()
        if usesExtension {
            tunnel?.connection.stopVPNTunnel()
            NSApp.terminate(nil)
            return
        }
        DispatchQueue.global(qos: .utility).async { [weak self] in
            _ = LocalCoreIPC.sendQuit()
            DispatchQueue.main.async {
                self?.publisherProcess?.terminate()
                NSApp.terminate(nil)
            }
        }
    }

    @objc private func quitMenu() {
        allowTerminate = true
        animTimer?.invalidate()
        animTimer = nil
        NSApp.terminate(nil)
    }

    @objc private func preferenceChanged(_ notification: Notification) {
        let enabled = MenuBarPreference.local.refresh()
        if usesExtension {
            if !enabled { quitMenu() }
            return
        }
        if enabled {
            showStatusItem()
            syncStatus()
        } else {
            hideStatusItem()
            if !publisherRunning { quitMenu() }
        }
    }

    @objc private func statusNotified(_ notification: Notification) {
        if let raw = notification.userInfo?["state"] as? Int {
            switch raw {
            case 2: setMark(.connected)
            case 1: setMark(.connecting)
            default: setMark(.idle)
            }
        } else {
            syncStatus()
        }
    }
}
