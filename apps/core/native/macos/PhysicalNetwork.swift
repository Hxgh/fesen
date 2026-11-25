import Foundation
import Network

final class PhysicalNetwork {
    private var monitors: [NWPathMonitor] = []
    private var paths: [String: String] = [:]

    func start(on queue: DispatchQueue, changed: @escaping () -> Void) {
        for (key, type) in [("wifi", NWInterface.InterfaceType.wifi), ("ethernet", .wiredEthernet)] {
            let monitor = NWPathMonitor(requiredInterfaceType: type)
            monitor.pathUpdateHandler = { [weak self] path in
                guard let self else { return }
                let interfaces = path.availableInterfaces.filter { $0.type == type }.map { $0.name }
                let gateways = path.gateways.map { String(describing: $0) }
                let fingerprint = "\(path.status)/\(path.supportsIPv4)/\(path.supportsIPv6)/\(interfaces.sorted())/\(gateways.sorted())"
                if let before = self.paths.updateValue(fingerprint, forKey: key), before != fingerprint {
                    changed()
                }
            }
            monitor.start(queue: queue)
            monitors.append(monitor)
        }
    }

    func stop() {
        monitors.forEach { $0.cancel() }
        monitors.removeAll()
        paths.removeAll()
    }
}
