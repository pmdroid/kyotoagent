import Foundation

nonisolated public struct PairingClient: Codable, Equatable, Sendable {
    public var name: String
    public var version: String

    public init(name: String, version: String) {
        self.name = name
        self.version = version
    }

    public static var ios: PairingClient {
        PairingClient(name: "Kyoto Agent iOS", version: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1.0")
    }
}

nonisolated public struct PairingServer: Codable, Equatable, Sendable {
    public var name: String
    public var version: String
    public var workspace: String
    public var model: String
    public var effort: String?
    public var yolo: Bool
    public var enhance: Bool
    public var show_closeout: Bool
    public var profile: String?
}

nonisolated public struct PairingInfo: Codable, Equatable, Sendable {
    public var access_token: String?
    public var server: PairingServer
    public var client: PairingClient
    public var models: [Model]
    public var repositories: [Project]

    public func confirms(_ identity: PairingClient) -> Bool {
        client == identity
            && !server.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !server.version.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && server.workspace.hasPrefix("/")
    }
}
