import Foundation

struct MenuBarPreference {
    static let local = MenuBarPreference(domain: "io.fesen.client")
    static let changed = Notification.Name("io.fesen.client.menuBarOnCloseChanged")
    static let statusChanged = Notification.Name("io.fesen.client.statusChanged")
    static let ensurePublisher = Notification.Name("io.fesen.client.ensurePublisher")
    private let domain: CFString
    private let key = "menuBarOnClose" as CFString

    init(domain: String) { self.domain = domain as CFString }

    var enabled: Bool { (CFPreferencesCopyAppValue(key, domain) as? Bool) ?? true }

    func set(_ enabled: Bool) -> Bool {
        CFPreferencesSetAppValue(key, enabled ? kCFBooleanTrue : kCFBooleanFalse, domain)
        return CFPreferencesAppSynchronize(domain)
    }

    func refresh() -> Bool {
        _ = CFPreferencesAppSynchronize(domain)
        return enabled
    }

    static func menuAppURL(in bundle: URL) -> URL {
        bundle.standardizedFileURL.appendingPathComponent("Contents/Helpers/FesenMenu.app")
    }

    static func isMenu(_ executable: URL?, in bundle: URL) -> Bool {
        executable?.standardizedFileURL == menuAppURL(in: bundle).appendingPathComponent("Contents/MacOS/fesen-menu")
    }
}
