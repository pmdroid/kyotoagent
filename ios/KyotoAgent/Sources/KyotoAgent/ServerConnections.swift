import Foundation
import Observation

nonisolated public struct SavedServer: Codable, Identifiable, Equatable, Sendable {
    public var id: String
    public var connection: String
    public var name: String?

    public init(id: String = UUID().uuidString, connection: String, name: String? = nil) {
        self.id = id
        self.connection = connection
        self.name = name
    }

    public var title: String { name ?? address }

    public var address: String {
        PairingConnection(connection)?.baseURL.absoluteString ?? ""
    }
}

nonisolated private struct SavedConnections: Codable {
    var servers: [SavedServer]
    var activeID: String?
}

@MainActor
@Observable
public final class ServerConnections {
    public private(set) var servers: [SavedServer]
    public private(set) var activeID: String?
    public private(set) var model: AppModel
    public private(set) var connecting = false
    public private(set) var failure: String?
    public var showingServers = false
    public var address = ""
    private var launchConnection: Swift.Task<Void, Never>?
    private let store: any BaseURLStoring
    private let makeModel: @MainActor (SavedServer) -> AppModel

    public init(
        store: any BaseURLStoring,
        legacy: any BaseURLStoring,
        makeModel: @escaping @MainActor (SavedServer) -> AppModel
    ) {
        self.store = store
        self.makeModel = makeModel
        let saved = (try? store.load()).flatMap { text in
            try? JSONDecoder().decode(SavedConnections.self, from: Data(text.utf8))
        }
        let initial: SavedConnections
        if let saved {
            initial = saved
        } else if let connection = try? legacy.load(), PairingConnection(connection) != nil {
            let server = SavedServer(id: "legacy", connection: connection)
            initial = SavedConnections(servers: [server], activeID: server.id)
        } else {
            initial = SavedConnections(servers: [], activeID: nil)
        }
        servers = initial.servers
        activeID = initial.activeID
        let selected = initial.servers.first { $0.id == initial.activeID } ?? SavedServer(id: "unconnected", connection: "")
        model = makeModel(selected)
    }

    public func connectOnLaunch() async {
        if let launchConnection {
            await launchConnection.value
            return
        }
        let connection = servers.first { $0.id == activeID }?.connection
        let task = Swift.Task { @MainActor in
            guard let connection, !model.connected, !connecting else { return }
            await connect(connection)
        }
        launchConnection = task
        await task.value
    }

    public func connect(_ text: String) async {
        guard !connecting else { return }
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let connection = PairingConnection(text) else {
            failure = "Scan a valid pairing QR code"
            return
        }
        address = text
        if !model.connected { model.baseURLText = text }
        if model.connected, model.baseURLText == text {
            failure = nil
            showingServers = false
            return
        }
        connecting = true
        failure = nil
        defer { connecting = false }
        var server = servers.first { $0.address == connection.baseURL.absoluteString }
            ?? SavedServer(connection: text)
        server.connection = text
        let candidate = makeModel(server)
        await candidate.connect()
        server.connection = candidate.baseURLText
        address = candidate.baseURLText
        if !model.connected { model.baseURLText = candidate.baseURLText }
        guard candidate.connected else {
            if server.connection != text {
                var updated = servers.filter { $0.id != server.id }
                updated.append(server)
                guard save(updated, activeID: activeID) else { return }
            }
            failure = candidate.failure
            return
        }
        var updated = servers.filter { $0.id != server.id }
        updated.append(server)
        guard save(updated, activeID: server.id) else { return }
        model = candidate
        showingServers = false
    }

    public func rename(_ id: String, name: String) {
        guard !connecting, let index = servers.firstIndex(where: { $0.id == id }) else { return }
        var updated = servers
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        updated[index].name = trimmed.isEmpty ? nil : trimmed
        _ = save(updated, activeID: activeID)
    }

    public func remove(_ id: String) {
        guard !connecting, let server = servers.first(where: { $0.id == id }) else { return }
        let removingActive = activeID == id
        guard save(servers.filter { $0.id != id }, activeID: removingActive ? nil : activeID) else { return }
        if removingActive {
            model = makeModel(SavedServer(id: "unconnected", connection: ""))
        }
        if PairingConnection(address)?.baseURL.absoluteString == server.address {
            address = ""
        }
    }

    private func save(_ updated: [SavedServer], activeID: String?) -> Bool {
        do {
            let saved = SavedConnections(servers: updated, activeID: activeID)
            let data = try JSONEncoder().encode(saved)
            try store.save(String(decoding: data, as: UTF8.self))
        } catch {
            failure = "Could not save the server securely. Try again."
            return false
        }
        servers = updated
        self.activeID = activeID
        failure = nil
        return true
    }
}
