import Foundation

struct MenuBarPreference {
    static let local = MenuBarPreference(domain: "io.fesen.client")
    static let changed = Notification.Name("io.fesen.client.menuBarOnCloseChanged")
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

    static func isMenu(_ executable: URL?, in bundle: URL) -> Bool {
        executable?.standardizedFileURL == bundle.standardizedFileURL.appendingPathComponent("Contents/MacOS/fesen-menu")
    }
}
