import AppKit

@MainActor class Accessory: NSObject, NSApplicationDelegate {
    let bundle: URL

    init(bundle: URL) { self.bundle = bundle.standardizedFileURL }

    func run() {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        app.delegate = self
        withExtendedLifetime(self) { app.run() }
    }

    func instance(_ policy: NSApplication.ActivationPolicy) -> NSRunningApplication? {
        let executable = Bundle(url: bundle)?.executableURL?.standardizedFileURL
        return NSRunningApplication.runningApplications(withBundleIdentifier: "io.fesen.client").first {
            !$0.isTerminated && $0.processIdentifier != ProcessInfo.processInfo.processIdentifier &&
                $0.activationPolicy == policy && $0.bundleURL?.standardizedFileURL == bundle &&
                executable != nil && $0.executableURL?.standardizedFileURL == executable
        }
    }

    @objc func openApp() {
        if instance(.regular)?.activate() == true {
            NSApp.terminate(nil)
            return
        }
        let config = NSWorkspace.OpenConfiguration()
        config.createsNewApplicationInstance = true
        NSWorkspace.shared.openApplication(at: bundle, configuration: config) { _, _ in
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        openApp()
        return false
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        // The cold GUI receives the open event; an existing GUI receives the join via its singleton.
        guard let url = urls.first(where: { $0.scheme == "fesen" && $0.host == "join" }),
              let link = URLComponents(url: url, resolvingAgainstBaseURL: false),
              (link.path.isEmpty || link.path == "/"), link.user == nil, link.password == nil,
              link.port == nil, link.fragment == nil, link.queryItems?.count == 1,
              let item = link.queryItems?.first, item.name == "server",
              let value = item.value, let server = URLComponents(string: value),
              ["http", "https"].contains(server.scheme), server.host != nil,
              server.user == nil, server.password == nil, server.query == nil, server.fragment == nil,
              (server.path.isEmpty || server.path == "/"), url.absoluteString.utf8.count <= 1024 else { return }
        let config = NSWorkspace.OpenConfiguration()
        config.createsNewApplicationInstance = true
        config.arguments = ["--fesen-join", url.absoluteString]
        NSWorkspace.shared.open([url], withApplicationAt: bundle, configuration: config) { _, _ in
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }
    }
}
