import Foundation
import NetworkExtension

private struct Settings: Decodable {
    let address: String?
    let dns: String?
    let routes: [String]

    func network() -> NEPacketTunnelNetworkSettings? {
        guard let address else { return nil }
        let settings = NEPacketTunnelNetworkSettings(tunnelRemoteAddress: "127.0.0.1")
        let ipv4 = NEIPv4Settings(addresses: [address], subnetMasks: ["255.255.255.255"])
        ipv4.includedRoutes = routes.compactMap { cidr in
            let parts = cidr.split(separator: "/")
            guard parts.count == 2, let prefix = UInt32(parts[1]), (1...32).contains(prefix) else { return nil }
            let mask = UInt32.max << (32 - prefix)
            let text = [24, 16, 8, 0].map { String((mask >> $0) & 255) }.joined(separator: ".")
            return NEIPv4Route(destinationAddress: String(parts[0]), subnetMask: text)
        }
        settings.ipv4Settings = ipv4
        settings.mtu = 1500
        if let dns {
            let resolver = NEDNSSettings(servers: [dns])
            resolver.matchDomains = ["fesen"]
            resolver.matchDomainsNoSearch = true
            settings.dnsSettings = resolver
        }
        return settings
    }
}

private struct Failure: LocalizedError {
    var errorDescription: String? { "飞弦（Fesen）核心无法启动" }
}

private final class Reply {
    let send: (Data?) -> Void
    init(_ send: @escaping (Data?) -> Void) { self.send = send }
}

private func provider(_ token: UInt) -> Tunnel {
    Unmanaged<Tunnel>.fromOpaque(UnsafeMutableRawPointer(bitPattern: token)!).takeUnretainedValue()
}

final class Tunnel: NEPacketTunnelProvider {
    private static let ipv4Protocols = [NSNumber(value: AF_INET)]
    private var engine: UnsafeMutableRawPointer?
    private let lane = DispatchQueue(label: "io.fesen.tunnel.core")
    private let physical = PhysicalNetwork()
    private var reading = false

    override func startTunnel(options: [String: NSObject]?, completionHandler: @escaping (Error?) -> Void) {
        lane.async { [self] in
            guard let profile = (self.protocolConfiguration as? NETunnelProviderProtocol)?
                .providerConfiguration?["profile"] as? String,
                profile.count == 32, profile.unicodeScalars.allSatisfy({ "0123456789abcdef".unicodeScalars.contains($0) }) else {
                completionHandler(Failure()); return
            }
            let path = "/Library/Application Support/fesen/\(profile)"
            let token = UInt(bitPattern: Unmanaged.passUnretained(self).toOpaque())
            self.engine = path.withCString {
                ph_core_start($0, token, { token, bytes, len in
                    guard let bytes, let settings = try? JSONDecoder().decode(Settings.self, from: Data(bytes: bytes, count: len))
                        else { return 0 }
                    return provider(token).adjust(settings)
                }, { token, bytes, len in
                    guard let bytes else { return }
                    let tunnel = provider(token)
                    _ = tunnel.packetFlow.writePackets([Data(bytes: bytes, count: len)], withProtocols: Tunnel.ipv4Protocols)
                }, { token in
                    let tunnel = provider(token)
                    tunnel.lane.async {
                        if tunnel.engine != nil { tunnel.cancelTunnelWithError(nil) }
                    }
                })
            }
            guard self.engine != nil else { completionHandler(Failure()); return }
            self.physical.start(on: self.lane) { [weak self] in
                if let engine = self?.engine { ph_core_changed(engine) }
            }
            completionHandler(nil)
        }
    }

    private func adjust(_ settings: Settings) -> Int32 {
        let settled = DispatchSemaphore(value: 0)
        var success = false
        setTunnelNetworkSettings(settings.network()) { error in
            success = error == nil
            settled.signal()
        }
        if settled.wait(timeout: .now() + 6) != .success || !success { return 0 }
        if settings.address != nil { lane.async { self.read() } }
        return 1
    }

    private func read() {
        guard engine != nil && !reading else { return }
        reading = true
        packetFlow.readPackets { packets, protocols in
            self.lane.async {
                self.reading = false
                guard let engine = self.engine else { return }
                for (packet, protocolNumber) in zip(packets, protocols) where protocolNumber.int32Value == AF_INET {
                    packet.withUnsafeBytes { ph_core_packet(engine, $0.baseAddress?.assumingMemoryBound(to: UInt8.self), $0.count) }
                }
                self.read()
            }
        }
    }

    override func handleAppMessage(_ messageData: Data, completionHandler: ((Data?) -> Void)?) {
        lane.async {
            guard let engine = self.engine, let completionHandler else { completionHandler?(nil); return }
            let token = UInt(bitPattern: Unmanaged.passRetained(Reply(completionHandler)).toOpaque())
            messageData.withUnsafeBytes {
                ph_core_call(engine, $0.baseAddress?.assumingMemoryBound(to: UInt8.self), $0.count, token) { token, bytes, count in
                    let reply = Unmanaged<Reply>.fromOpaque(UnsafeMutableRawPointer(bitPattern: token)!).takeRetainedValue()
                    reply.send(bytes.map { Data(bytes: $0, count: count) })
                }
            }
        }
    }

    override func stopTunnel(with reason: NEProviderStopReason, completionHandler: @escaping () -> Void) {
        lane.async {
            self.physical.stop()
            if let engine = self.engine {
                self.engine = nil
                ph_core_stop(engine)
            }
            completionHandler()
        }
    }
}
