import Foundation
import Observation

nonisolated public struct SavedServer: Codable, Identifiable, Equatable, Sendable {
    public static let legacyID = "00000000-0000-4000-8000-000000000001"
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
    public private(set) var models: [String: AppModel]
    public var visibleSessionID: String?
    public var sessionFilter: SessionListFilter = .all
    public private(set) var selectingSessions = false
    public private(set) var selectedSessions: Set<ServerItemID> = []
    public private(set) var sessionDeletion: SessionDeletionConfirmation?
    public private(set) var deletingSessions = false
    public private(set) var sessionDeletionFailures: [SessionDeletionFailure] = []
    public private(set) var sessionDeletionNotice: String?
    public private(set) var notificationRevision = 0
    public var notificationsChanged: (@MainActor () async -> Void)?
    public var serverRemoved: (@MainActor (SavedServer) -> Void)?
    private var selectionRevision = 0
    private var lastSessionDeletion: UUID?
    private var launchConnection: Swift.Task<Void, Never>?
    private var connectionTasks: [ObjectIdentifier: Swift.Task<Void, Never>] = [:]
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
        var initial: SavedConnections
        if let saved {
            initial = saved
        } else if let connection = try? legacy.load(), PairingConnection(connection) != nil {
            let server = SavedServer(id: SavedServer.legacyID, connection: connection)
            initial = SavedConnections(servers: [server], activeID: server.id)
        } else {
            initial = SavedConnections(servers: [], activeID: nil)
        }
        let migratingLegacy = initial.servers.contains { $0.id == "legacy" }
        for index in initial.servers.indices where initial.servers[index].id == "legacy" {
            initial.servers[index].id = SavedServer.legacyID
        }
        if initial.activeID == "legacy" { initial.activeID = SavedServer.legacyID }
        if migratingLegacy, let data = try? JSONEncoder().encode(initial) {
            try? store.save(String(decoding: data, as: UTF8.self))
        }
        servers = initial.servers
        activeID = initial.activeID
        let selected = initial.servers.first { $0.id == initial.activeID } ?? SavedServer(id: "unconnected", connection: "")
        let loaded = Dictionary(uniqueKeysWithValues: initial.servers.map { ($0.id, makeModel($0)) })
        models = loaded
        model = loaded[selected.id] ?? makeModel(selected)
    }

    public func connectOnLaunch() async {
        if let launchConnection {
            await launchConnection.value
            return
        }
        let task = Swift.Task { @MainActor in
            guard !servers.isEmpty else { return }
            let preferredID = activeID
            let revision = selectionRevision
            connecting = true
            defer { connecting = false }
            await withTaskGroup(of: Void.self) { group in
                for server in servers {
                    group.addTask { await self.reconnect(server.id, selectIfNeeded: true) }
                }
            }
            if revision == selectionRevision, let preferredID { activateServer(preferredID) }
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
        let candidate = models[server.id].flatMap { $0.baseURLText == text ? $0 : nil } ?? makeModel(server)
        await ensureConnected(candidate)
        server.connection = candidate.baseURLText
        address = candidate.baseURLText
        if !model.connected { model.baseURLText = candidate.baseURLText }
        guard candidate.connected else {
            if server.connection != text {
                var updated = servers.filter { $0.id != server.id }
                updated.append(server)
                guard save(updated, activeID: activeID) else { return }
                models[server.id] = candidate
            }
            failure = candidate.failure
            return
        }
        var updated = servers.filter { $0.id != server.id }
        updated.append(server)
        guard save(updated, activeID: server.id) else { return }
        models[server.id] = candidate
        model = candidate
        await candidate.loadProjects(resetCreation: false)
        showingServers = false
        await notificationsChanged?()
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
        let remaining = servers.filter { $0.id != id }
        let next = remaining.first { models[$0.id]?.connected == true }
        guard save(remaining, activeID: removingActive ? next?.id : activeID) else { return }
        serverRemoved?(server)
        models.removeValue(forKey: id)
        if removingActive {
            model = next.flatMap { models[$0.id] } ?? makeModel(SavedServer(id: "unconnected", connection: ""))
        }
        if PairingConnection(address)?.baseURL.absoluteString == server.address {
            address = ""
        }
    }

    public func selectServer(_ id: String) {
        selectionRevision += 1
        activateServer(id)
    }

    private func activateServer(_ id: String) {
        guard let selected = models[id], selected.connected else { return }
        guard activeID != id || model !== selected else { return }
        guard save(servers, activeID: id) else { return }
        model = selected
    }

    @discardableResult
    public func openSession(serverID: String, sessionID: String) -> Bool {
        guard models[serverID]?.connected == true else { return false }
        selectServer(serverID)
        guard activeID == serverID else { return false }
        model.open(sessionID)
        return true
    }

    public func openNotification(_ route: NotificationRoute) async -> Bool {
        guard servers.contains(where: { $0.id == route.serverID }) else {
            failure = "This notification's server is no longer saved."
            return false
        }
        await reconnect(route.serverID)
        guard let candidate = models[route.serverID], candidate.connected else {
            failure = "The notification's server is offline. Try again when it is reachable."
            return false
        }
        await candidate.refreshSessions()
        guard candidate.sessions.contains(where: { $0.id == route.sessionID }) else {
            failure = "This session is unavailable or has been deleted."
            return false
        }
        guard openSession(serverID: route.serverID, sessionID: route.sessionID) else { return false }
        showingServers = false
        notificationRevision += 1
        await candidate.refreshOpenView()
        return true
    }

    private func reconnect(_ id: String, selectIfNeeded: Bool = false) async {
        guard let candidate = models[id], let server = servers.first(where: { $0.id == id }) else { return }
        await ensureConnected(candidate)
        guard models[id] === candidate, let index = servers.firstIndex(where: { $0.id == id }) else { return }
        if candidate.baseURLText != server.connection {
            var updated = servers
            updated[index].connection = candidate.baseURLText
            guard save(updated, activeID: activeID) else { return }
        }
        if candidate.connected {
            if activeID == id || (selectIfNeeded && !model.connected) { activateServer(id) }
            await candidate.loadProjects(resetCreation: false)
        } else if !model.connected {
            failure = candidate.failure
        }
    }

    private func ensureConnected(_ candidate: AppModel) async {
        guard !candidate.connected else { return }
        let id = ObjectIdentifier(candidate)
        if let pending = connectionTasks[id] {
            await pending.value
            return
        }
        let pending = Swift.Task { @MainActor in await candidate.connect() }
        connectionTasks[id] = pending
        await pending.value
        connectionTasks.removeValue(forKey: id)
    }

    public func beginSessionSelection() {
        guard !deletingSessions else { return }
        selectingSessions = true
        selectedSessions = []
        sessionDeletionFailures = []
        sessionDeletionNotice = nil
    }

    public func cancelSessionSelection() {
        guard !deletingSessions else { return }
        selectingSessions = false
        selectedSessions = []
        sessionDeletion = nil
        sessionDeletionFailures = []
        sessionDeletionNotice = nil
    }

    public func updateSelectedSessions(_ ids: Set<ServerItemID>) {
        guard selectingSessions, !deletingSessions, sessionDeletion == nil else { return }
        selectedSessions = ids
    }

    public func toggleSessionSelection(_ id: ServerItemID) {
        guard selectingSessions, !deletingSessions, sessionDeletion == nil else { return }
        if selectedSessions.contains(id) { selectedSessions.remove(id) } else { selectedSessions.insert(id) }
    }

    private func deletionTarget(_ id: ServerItemID) -> SessionDeletionTarget? {
        guard let candidate = models[id.serverID], let session = candidate.sessions.first(where: { $0.id == id.itemID }),
              let server = servers.first(where: { $0.id == id.serverID }) else { return nil }
        return SessionDeletionTarget(id: id, serverTitle: server.title, title: session.title ?? session.id, model: candidate)
    }

    public func requestSelectedSessionDeletion() {
        guard !deletingSessions else { return }
        let retries = Dictionary(uniqueKeysWithValues: sessionDeletionFailures.map { ($0.target.id, $0.target) })
        let targets = selectedSessions.compactMap { deletionTarget($0) ?? retries[$0] }
            .sorted { ($0.id.serverID, $0.id.itemID ?? "") < ($1.id.serverID, $1.id.itemID ?? "") }
        guard !targets.isEmpty else { return }
        sessionDeletion = SessionDeletionConfirmation(targets: targets, serverTitle: nil)
    }

    public var currentServerArchivedCount: Int { model.sessions.filter(\.archived).count }

    public func requestArchivedSessionDeletion() {
        guard !deletingSessions, let activeID, let server = servers.first(where: { $0.id == activeID }) else { return }
        let targets = model.sessions.filter(\.archived).compactMap {
            deletionTarget(ServerItemID(serverID: activeID, itemID: $0.id))
        }
        guard !targets.isEmpty else { return }
        sessionDeletion = SessionDeletionConfirmation(targets: targets, serverTitle: server.title)
    }

    public func cancelSessionDeletion() {
        guard !deletingSessions else { return }
        sessionDeletion = nil
    }

    public func confirmSessionDeletion(_ confirmation: SessionDeletionConfirmation) async {
        guard !deletingSessions, lastSessionDeletion != confirmation.id else { return }
        lastSessionDeletion = confirmation.id
        deletingSessions = true
        selectingSessions = true
        sessionDeletion = nil
        sessionDeletionFailures = []
        sessionDeletionNotice = nil
        defer { deletingSessions = false }
        var failures: [SessionDeletionFailure] = []
        var affected: [String: AppModel] = [:]
        for target in confirmation.targets {
            guard let sessionID = target.id.itemID else { continue }
            affected[target.id.serverID] = target.model
            do {
                try await target.model.deleteSessionWithoutRefresh(sessionID)
            } catch let error as HostError {
                failures.append(SessionDeletionFailure(target: target, message: error.serverMessage))
            } catch {
                failures.append(SessionDeletionFailure(target: target, message: error.localizedDescription))
            }
        }
        for candidate in affected.values { await candidate.refreshSessions() }
        sessionDeletionFailures = failures
        selectedSessions = Set(failures.map { $0.target.id })
        selectingSessions = !failures.isEmpty
        let deleted = confirmation.targets.count - failures.count
        sessionDeletionNotice = failures.isEmpty
            ? "Deleted \(deleted) sessions. Worktrees and files were kept."
            : "Deleted \(deleted) of \(confirmation.targets.count) sessions. \(failures.count) could not be deleted. Retry the selected sessions."
        await notificationsChanged?()
    }

    public func pollSessions() async {
        await withTaskGroup(of: Void.self) { group in
            for server in servers {
                group.addTask { await self.pollServer(server.id) }
            }
        }
    }

    private func pollServer(_ id: String) async {
        while !Swift.Task.isCancelled {
            guard let candidate = models[id] else { return }
            if candidate.connected {
                await candidate.refreshSessions()
                await notificationsChanged?()
            } else {
                await reconnect(id)
            }
            try? await Swift.Task.sleep(for: RefreshCadence.sessions)
        }
    }

    public func loadProjects() async {
        await withTaskGroup(of: Void.self) { group in
            for candidate in models.values where candidate.connected {
                group.addTask { await candidate.loadProjects(resetCreation: false) }
            }
        }
    }

    public var projects: [ServerProject] {
        servers.flatMap { server in
            guard let candidate = models[server.id] else { return [ServerProject]() }
            return candidate.projects.map { ServerProject(server: server, project: $0) }
        }
    }

    public var projectGroups: [ServerProjectGroup] {
        servers.flatMap { server in
            guard let candidate = models[server.id] else { return [ServerProjectGroup]() }
            var ids = candidate.projects.map { Optional($0.id) }
            for session in candidate.sessions where !ids.contains(session.project) {
                ids.append(session.project)
            }
            return ids.map { id in
                ServerProjectGroup(
                    server: server,
                    projectID: id,
                    name: candidate.projects.first { $0.id == id }?.name ?? id ?? "Other",
                    model: candidate,
                    nodes: nestedSessions(filteredSessions(candidate.sessions.filter { $0.project == id }, filter: sessionFilter))
                )
            }
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

nonisolated public struct ServerItemID: Hashable, Sendable {
    public var serverID: String
    public var itemID: String?
}

nonisolated public struct SessionDeletionTarget: Identifiable, Sendable {
    public let id: ServerItemID
    public let serverTitle: String
    public let title: String
    public let model: AppModel
}

nonisolated public struct SessionDeletionConfirmation: Identifiable, Sendable {
    public let id = UUID()
    public let targets: [SessionDeletionTarget]
    public let serverTitle: String?
    public var title: String { "Delete \(targets.count) sessions?" }
    public var message: String {
        let scope = serverTitle.map { "These archived sessions are on \($0). " } ?? ""
        return scope + "This permanently deletes the selected sessions. Worktrees and files will be kept."
    }
}

nonisolated public struct SessionDeletionFailure: Identifiable, Sendable {
    public var target: SessionDeletionTarget
    public var message: String
    public var id: ServerItemID { target.id }
}

nonisolated public struct ServerProject: Identifiable, Sendable {
    public var server: SavedServer
    public var project: Project
    public var id: ServerItemID { ServerItemID(serverID: server.id, itemID: project.id) }
}

nonisolated public struct ServerProjectGroup: Identifiable, Sendable {
    public var server: SavedServer
    public var projectID: String?
    public var name: String
    public var model: AppModel
    public var nodes: [SessionNode]
    public var id: ServerItemID { ServerItemID(serverID: server.id, itemID: projectID) }
}
