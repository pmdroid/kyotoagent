import Foundation

nonisolated public enum PushEnvironment: String, Codable, Sendable {
    case sandbox
    case production
}

nonisolated public struct DeviceRegistration: Codable, Equatable, Sendable {
    public var id: String
    public var token: String
    public var environment: PushEnvironment
    public var serverId: String

    public init(id: String, token: Data, environment: PushEnvironment, serverId: String) {
        self.id = id
        self.token = token.map { String(format: "%02x", $0) }.joined()
        self.environment = environment
        self.serverId = serverId
    }
}

nonisolated struct DeviceDeletion: Encodable {
    var id: String
}

nonisolated public enum NotificationPermission: Equatable, Sendable {
    case notDetermined
    case denied
    case authorized

    public func shouldRegister(enabled: Bool) -> Bool {
        enabled && self == .authorized
    }
}

nonisolated public struct NotificationRoute: Equatable, Sendable {
    public let serverID: String
    public let sessionID: String

    public init?(payload: [AnyHashable: Any]) {
        guard let server = payload["serverId"] as? String, !server.isEmpty,
              let session = payload["sessionId"] as? String, sessionFileName(session) != nil else { return nil }
        serverID = server
        sessionID = session
    }

    public func suppress(activeServerID: String?, openSessionID: String?) -> Bool {
        serverID == activeServerID && sessionID == openSessionID
    }
}

extension PushEnvironment {
    public static func signedExecutable(_ data: Data) -> PushEnvironment? {
        func integer(_ offset: Int, bigEndian: Bool) -> UInt32? {
            guard offset >= 0, offset <= data.count - 4 else { return nil }
            let bytes = data[offset..<(offset + 4)]
            return (bigEndian ? Array(bytes) : Array(bytes.reversed())).reduce(0) { ($0 << 8) | UInt32($1) }
        }
        guard let magic = integer(0, bigEndian: false), magic == 0xfeedfacf || magic == 0xfeedface,
              let count = integer(16, bigEndian: false), count <= 4096 else { return nil }
        var command = magic == 0xfeedfacf ? 32 : 28
        for _ in 0..<count {
            guard let kind = integer(command, bigEndian: false),
                  let size = integer(command + 4, bigEndian: false), size >= 8,
                  command <= data.count - Int(size) else { return nil }
            if kind == 0x1d {
                guard size >= 16, let offset = integer(command + 8, bigEndian: false),
                      let length = integer(command + 12, bigEndian: false) else { return nil }
                let base = Int(offset)
                guard Int(length) >= 12, base <= data.count - Int(length),
                      integer(base, bigEndian: true) == 0xfade0cc0,
                      let blobLength = integer(base + 4, bigEndian: true), blobLength <= length,
                      let blobs = integer(base + 8, bigEndian: true),
                      Int(blobs) <= (Int(blobLength) - 12) / 8 else { return nil }
                for index in 0..<Int(blobs) {
                    let entry = base + 12 + index * 8
                    guard let slot = integer(entry, bigEndian: true),
                          let relative = integer(entry + 4, bigEndian: true) else { return nil }
                    if slot != 5 { continue }
                    let start = base + Int(relative)
                    guard Int(relative) <= Int(blobLength) - 8,
                          integer(start, bigEndian: true) == 0xfade7171,
                          let size = integer(start + 4, bigEndian: true), size >= 8,
                          Int(relative) + Int(size) <= Int(blobLength),
                          let plist = try? PropertyListSerialization.propertyList(
                            from: data.subdata(in: (start + 8)..<(start + Int(size))), options: [], format: nil
                          ) as? [String: Any],
                          let value = plist["aps-environment"] as? String else { return nil }
                    switch value {
                    case "development": return .sandbox
                    case "production": return .production
                    default: return nil
                    }
                }
                return nil
            }
            command += Int(size)
        }
        return nil
    }
}
