import AppKit
import Darwin
import Foundation
import NetworkExtension
import Security
import SystemExtensions

private let providerID = "io.fesen.client.tunnel"

// The CSS viewport begins at the content inset of the main web view, not at the window frame.
@_cdecl("ph_site_viewport_top")
func phSiteViewportTop(_ ptr: UnsafeMutableRawPointer) -> Int32 {
    let view = Unmanaged<NSView>.fromOpaque(ptr).takeUnretainedValue()
    guard let parent = view.superview, let window = view.window else { return 0 }
    let rect = view.convert(view.bounds, to: parent)
    let top = parent.isFlipped ? rect.minY - parent.bounds.minY : parent.bounds.maxY - rect.maxY
    let contentInset = view.enclosingScrollView?.contentInsets.top ?? 0
    let windowInset = window.styleMask.contains(.fullSizeContentView)
        ? 0
        : window.frame.height - window.contentLayoutRect.maxY
    return Int32((max(top + contentInset, windowInset, 0) * window.backingScaleFactor).rounded())
}

enum HostFailure: String, Error {
    case offline = "not_connected"
    case extensionSetup = "extension_setup"
    case extensionPermission = "extension_permission"
    case extensionUnsigned = "extension_unsigned"
    case extensionFailed = "extension_failed"
    case extensionLocation = "extension_location"
    case extensionRestart = "extension_restart"
}

private func signingInfo(_ url: URL) -> [String: Any]? {
    var code: SecStaticCode?
    var info: CFDictionary?
    guard SecStaticCodeCreateWithPath(url as CFURL, [], &code) == errSecSuccess, let code,
          SecCodeCopySigningInformation(code, SecCSFlags(rawValue: kSecCSSigningInformation), &info) == errSecSuccess
    else { return nil }
    return info as? [String: Any]
}

private func packageFailure() -> HostFailure? {
    let app = Bundle.main.bundleURL
    let tunnel = app.appendingPathComponent("Contents/Library/SystemExtensions/\(providerID).systemextension")
    let executableEntitlements = kSecCodeInfoEntitlementsDict as String
    let team = kSecCodeInfoTeamIdentifier as String
    let network = "com.apple.developer.networking.networkextension"
    let capability = "packet-tunnel-provider-systemextension"
    let profile = "Contents/embedded.provisionprofile"
    guard FileManager.default.fileExists(atPath: app.appendingPathComponent(profile).path),
          FileManager.default.fileExists(atPath: tunnel.appendingPathComponent(profile).path),
          let appInfo = signingInfo(app), let tunnelInfo = signingInfo(tunnel),
          let appTeam = appInfo[team] as? String, !appTeam.isEmpty, appTeam == tunnelInfo[team] as? String,
          let host = appInfo[executableEntitlements] as? [String: Any],
          let provider = tunnelInfo[executableEntitlements] as? [String: Any],
          host["com.apple.developer.system-extension.install"] as? Bool == true,
          (host[network] as? [String])?.contains(capability) == true,
          (provider[network] as? [String])?.contains(capability) == true
    else { return .extensionUnsigned }
    return nil
}

private let invalidPackage = packageFailure()

@_cdecl("ph_host_can_route")
func hostCanRoute() -> Int32 { invalidPackage == nil ? 1 : 0 }

@_cdecl("ph_host_notify_status")
func hostNotifyStatus(_ state: Int32) {
    let path = Bundle.main.bundleURL.standardizedFileURL.path
    DistributedNotificationCenter.default().postNotificationName(
        MenuBarPreference.statusChanged,
        object: path,
        userInfo: ["state": Int(state)],
        deliverImmediately: true
    )
}

@MainActor
final class AppleHost {
    static let shared = AppleHost()

    private var manager: NETunnelProviderManager?
    private let activation = Activation()
    private var installation: Task<NETunnelProviderManager, Error>?
    private var tray: Process?

    func find() async throws -> NETunnelProviderManager? {
        if let manager { return manager }
        if let existing = try await NETunnelProviderManager.loadAllFromPreferences().first(where: {
            ($0.protocolConfiguration as? NETunnelProviderProtocol)?.providerBundleIdentifier == providerID
        }) {
            manager = existing
        }
        return manager
    }

    private func install(_ existing: NETunnelProviderManager?) async throws -> NETunnelProviderManager {
        if let installation { return try await installation.value }
        let task = Task {
            try await activation.activate()
            let manager = existing ?? NETunnelProviderManager()
            let proto = (manager.protocolConfiguration as? NETunnelProviderProtocol) ?? NETunnelProviderProtocol()
            proto.providerBundleIdentifier = providerID
            proto.serverAddress = "fesen"
            if proto.providerConfiguration?["profile"] == nil {
                let profile = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
                proto.providerConfiguration = ["profile": profile]
            }
            manager.protocolConfiguration = proto
            manager.localizedDescription = "Fesen"
            manager.isEnabled = true
            do {
                try await manager.saveToPreferences()
                try await manager.loadFromPreferences()
                guard manager.isEnabled else { throw HostFailure.extensionFailed }
            } catch {
                self.manager = nil
                throw HostFailure.extensionFailed
            }
            self.manager = manager
            return manager
        }
        installation = task
        defer { installation = nil }
        return try await task.value
    }

    func call(_ request: Data, launch: Bool, attaching: Bool = false) async throws -> Data {
        if launch || attaching, let invalidPackage { throw invalidPackage }
        let stored: NETunnelProviderManager?
        do { stored = try await find() }
        catch { throw launch || attaching ? HostFailure.extensionFailed : HostFailure.offline }
        let manager: NETunnelProviderManager
        if let stored, stored.isEnabled {
            manager = stored
        } else if launch && attaching {
            manager = try await install(stored)
        } else if attaching {
            throw activation.blocker ?? HostFailure.extensionSetup
        } else {
            throw HostFailure.offline
        }
        guard let session = manager.connection as? NETunnelProviderSession else { throw HostFailure.offline }
        if launch && [.invalid, .disconnected].contains(session.status) {
            try session.startVPNTunnel()
        }
        if launch {
            for _ in 0..<100 where [.connecting, .reasserting, .disconnecting, .disconnected].contains(session.status) {
                if session.status == .connected { break }
                try await Task.sleep(nanoseconds: 100_000_000)
            }
        }
        guard session.status == .connected else { throw HostFailure.offline }
        return try await withCheckedThrowingContinuation { continuation in
            do {
                try session.sendProviderMessage(request) { reply in
                    if let reply { continuation.resume(returning: reply) }
                    else { continuation.resume(throwing: HostFailure.offline) }
                }
            } catch {
                continuation.resume(throwing: error)
            }
        }
    }

    func openTray(force: Bool = false) {
        guard force || MenuBarPreference.local.enabled else { closeTray(); return }
        for app in NSWorkspace.shared.runningApplications where MenuBarPreference.isMenu(app.executableURL, in: bundleURLIfAny()) {
            if !app.isTerminated { return }
        }
        if tray?.isRunning == true { return }
        let executable = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/fesen-menu")
        let process = Process()
        process.executableURL = executable
        process.arguments = [invalidPackage == nil ? "--extension" : "--local"]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        if (try? process.run()) != nil { tray = process }
    }

    private func bundleURLIfAny() -> URL { Bundle.main.bundleURL }

    func closeTray() {
        tray?.terminate()
        tray = nil
        // Reopened GUIs don't own the old Process handle. Match only the menu executable, never the core.
        let bundle = Bundle.main.bundleURL
        DistributedNotificationCenter.default().postNotificationName(MenuBarPreference.changed,
            object: bundle.standardizedFileURL.path, userInfo: nil, deliverImmediately: true)
        let running = NSWorkspace.shared.runningApplications.filter { MenuBarPreference.isMenu($0.executableURL, in: bundle) }
        for app in running {
            _ = app.terminate()
        }
        if !running.isEmpty {
            let deadline = Date().addingTimeInterval(0.45)
            while Date() < deadline && running.contains(where: { !$0.isTerminated }) {
                usleep(20_000)
            }
        }
    }
}

@_cdecl("ph_host_configure_window")
@MainActor func hostConfigureWindow(_ ptr: UnsafeMutableRawPointer) {
    let window = Unmanaged<NSWindow>.fromOpaque(ptr).takeUnretainedValue()
    window.titlebarAppearsTransparent = true
    window.titlebarSeparatorStyle = .none
    if let close = window.standardWindowButton(.closeButton),
       let titlebarView = close.superview {
        for sub in titlebarView.subviews where !(sub is NSControl) {
            sub.isHidden = true
        }
    }
}

@MainActor
private final class Activation: NSObject, @preconcurrency OSSystemExtensionRequestDelegate {
    private var request: OSSystemExtensionRequest?
    private var continuation: CheckedContinuation<Void, Error>?
    private var activated = false
    private(set) var blocker: HostFailure?

    func activate() async throws {
        if activated { return }
        if let blocker { throw blocker }
        try await withCheckedThrowingContinuation { continuation in
            let request = OSSystemExtensionRequest.activationRequest(forExtensionWithIdentifier: providerID, queue: .main)
            self.request = request
            self.continuation = continuation
            request.delegate = self
            OSSystemExtensionManager.shared.submitRequest(request)
        }
    }

    private func complete(_ failure: HostFailure?) {
        let continuation = self.continuation
        self.continuation = nil
        if let failure { continuation?.resume(throwing: failure) }
        else { continuation?.resume() }
    }

    func request(_ request: OSSystemExtensionRequest, actionForReplacingExtension existing: OSSystemExtensionProperties,
                 withExtension replacement: OSSystemExtensionProperties) -> OSSystemExtensionRequest.ReplacementAction { .replace }
    func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {
        guard self.request === request else { return }
        // The OS request remains live while the window can show the approval instructions.
        blocker = .extensionPermission
        complete(blocker)
    }
    func request(_ request: OSSystemExtensionRequest, didFinishWithResult result: OSSystemExtensionRequest.Result) {
        guard self.request === request else { return }
        self.request = nil
        activated = result == .completed
        blocker = activated ? nil : .extensionRestart
        complete(blocker)
    }
    func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {
        guard self.request === request else { return }
        self.request = nil
        blocker = nil
        let failure: HostFailure
        switch (error as? OSSystemExtensionError)?.code {
        case .missingEntitlement, .codeSignatureInvalid, .validationFailed:
            failure = .extensionUnsigned
        case .unsupportedParentBundleLocation:
            failure = .extensionLocation
        case .authorizationRequired:
            failure = .extensionPermission
        default:
            failure = .extensionFailed
        }
        complete(failure)
    }
}

typealias Callback = @convention(c) (UInt, UnsafePointer<UInt8>?, Int) -> Void

@_cdecl("ph_host_launch_publisher")
func hostLaunchPublisher(_ context: UInt, _ done: @escaping @convention(c) (UInt, Int32) -> Void) {
    DispatchQueue.main.async {
        let bundle = Bundle.main
        guard bundle.bundleIdentifier == "io.fesen.client",
              bundle.bundleURL.pathExtension == "app"
        else {
            done(context, 0); return
        }
        AppleHost.shared.openTray(force: true)
        DistributedNotificationCenter.default().postNotificationName(
            MenuBarPreference.ensurePublisher,
            object: bundle.bundleURL.standardizedFileURL.path,
            userInfo: nil,
            deliverImmediately: true
        )
        done(context, 1)
    }
}

@_cdecl("ph_host_call")
func hostCall(_ bytes: UnsafePointer<UInt8>?, _ count: Int, _ mode: Int32, _ context: UInt, _ callback: Callback) {
    let request = bytes.map { Data(bytes: $0, count: count) } ?? Data()
    Task { @MainActor in
        let reply: Data
        do {
            reply = try await AppleHost.shared.call(request, launch: mode == 1 || mode == 3, attaching: mode >= 2)
        } catch {
            let code = (error as? HostFailure)?.rawValue ?? HostFailure.offline.rawValue
            reply = Data("{\"error\":\"\(code)\"}".utf8)
        }
        reply.withUnsafeBytes { callback(context, $0.baseAddress?.assumingMemoryBound(to: UInt8.self), $0.count) }
    }
}

@_cdecl("ph_host_open_settings")
func hostOpenSettings() -> Int32 {
    let open: () -> Int32 = {
        let uri: String
        if #available(macOS 15.0, *) {
            uri = "x-apple.systempreferences:com.apple.LoginItems-Settings.extension"
        } else {
            uri = "x-apple.systempreferences:com.apple.preference.security?General"
        }
        let workspace = NSWorkspace.shared
        if let url = URL(string: uri), workspace.open(url) { return 1 }
        return workspace.open(URL(fileURLWithPath: "/System/Applications/System Settings.app")) ? 1 : 0
    }
    return Thread.isMainThread ? open() : DispatchQueue.main.sync(execute: open)
}

@_cdecl("ph_host_menu_bar_setting")
@MainActor func hostMenuBarSetting(_ value: Int32) -> Int32 {
    let preference = MenuBarPreference.local
    if value != -1 {
        guard (value == 0 || value == 1), preference.set(value == 1) else { return -1 }
        if value == 0 {
            if invalidPackage == nil {
                AppleHost.shared.closeTray()
            } else {
                DistributedNotificationCenter.default().postNotificationName(
                    MenuBarPreference.changed,
                    object: Bundle.main.bundleURL.standardizedFileURL.path,
                    userInfo: nil,
                    deliverImmediately: true
                )
            }
        } else {
            AppleHost.shared.openTray()
            DistributedNotificationCenter.default().postNotificationName(
                MenuBarPreference.changed,
                object: Bundle.main.bundleURL.standardizedFileURL.path,
                userInfo: nil,
                deliverImmediately: true
            )
        }
    }
    return preference.enabled ? 1 : 0
}

@_cdecl("ph_host_tray")
func hostTray() {
    if Thread.isMainThread {
        MainActor.assumeIsolated { AppleHost.shared.openTray() }
    } else {
        DispatchQueue.main.sync { AppleHost.shared.openTray() }
    }
}

@_cdecl("ph_host_close_tray")
func hostCloseTray() {
    if Thread.isMainThread {
        MainActor.assumeIsolated { AppleHost.shared.closeTray() }
    } else {
        DispatchQueue.main.sync { AppleHost.shared.closeTray() }
    }
}
