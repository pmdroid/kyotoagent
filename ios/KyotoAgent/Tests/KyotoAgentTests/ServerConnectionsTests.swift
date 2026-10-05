import XCTest
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
@testable import KyotoAgent

@MainActor
final class ServerConnectionsTests: XCTestCase {
    func testSwitchingSeparatesDraftsForIdenticalSessionIDsAndRestoresSelection() async throws {
        let store = MemoryBaseURL()
        let connections = makeConnections(store: store)
        await connections.connect("https://first.example")
        let first = connections.model
        let session = try XCTUnwrap(first.sessions.first?.id)
        first.open(session)
        first.updateDraft("First server draft")
        await connections.connect("https://second.example")
        XCTAssertFalse(connections.model === first)
        XCTAssertNil(connections.model.selection)
        connections.model.open(session)
        XCTAssertEqual(connections.model.draft, "")
        connections.model.updateDraft("Second server draft")
        await connections.connect("https://first.example")
        XCTAssertEqual(connections.model.selection, session)
        XCTAssertEqual(connections.model.draft, "First server draft")
        XCTAssertEqual(connections.servers.count, 2)
        await connections.connect("https://second.example")
        XCTAssertEqual(connections.model.draft, "Second server draft")
    }

    func testFailedSwitchKeepsTheCurrentConnectionAndDraft() async throws {
        let gate = goodGate()
        let store = MemoryBaseURL()
        let connections = makeConnections(store: store, gate: gate)
        await connections.connect("https://first.example")
        let first = connections.model
        first.open(try XCTUnwrap(first.sessions.first?.id))
        first.updateDraft("Keep this")
        let saved = store.value
        gate.pairHandler = { _ in HostResponse(status: 401, body: Data("Unauthorized".utf8)) }
        await connections.connect("https://unavailable.example")
        XCTAssertTrue(connections.model === first)
        XCTAssertTrue(connections.model.connected)
        XCTAssertEqual(connections.model.draft, "Keep this")
        XCTAssertEqual(connections.model.baseURLText, "https://first.example")
        XCTAssertEqual(connections.address, "https://unavailable.example")
        XCTAssertEqual(connections.servers.count, 1)
        XCTAssertEqual(store.value, saved)
        XCTAssertNotNil(connections.failure)
    }

    func testFailedFirstPairingKeepsTheAddressForRetry() async {
        let gate = goodGate()
        gate.pairHandler = { _ in HostResponse(status: 401, body: Data("Unauthorized".utf8)) }
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        let link = "kyotoagent://first.example:7841?token=test-token"
        connections.showingServers = true
        await connections.connect(link)
        XCTAssertFalse(connections.model.connected)
        XCTAssertEqual(connections.model.baseURLText, link)
        XCTAssertEqual(connections.address, link)
        XCTAssertTrue(connections.showingServers)
        XCTAssertNotNil(connections.failure)
    }

    func testLegacyConnectionIsImportedAndCatalogSurvivesRestart() async {
        let store = MemoryBaseURL()
        let legacy = MemoryBaseURL(value: "https://first.example")
        let connections = makeConnections(store: store, legacy: legacy)
        XCTAssertEqual(connections.servers.first?.id, "legacy")
        XCTAssertEqual(connections.model.baseURLText, "https://first.example")
        await connections.connect("https://second.example")
        let restored = makeConnections(store: store, legacy: legacy)
        XCTAssertEqual(restored.servers, connections.servers)
        XCTAssertEqual(restored.activeID, connections.activeID)
        XCTAssertEqual(restored.model.baseURLText, "https://second.example")
    }

    func testNewTokenUpdatesOneServerWithoutDisplayingCredentials() async throws {
        let connections = makeConnections(store: MemoryBaseURL())
        await connections.connect("kyotoagent://first.example:7841?token=old-token")
        let id = try XCTUnwrap(connections.activeID)
        await connections.connect("kyotoagent://first.example:7841?token=new-token")
        XCTAssertEqual(connections.activeID, id)
        XCTAssertEqual(connections.servers.count, 1)
        XCTAssertEqual(connections.servers[0].address, "https://first.example:7841")
        XCTAssertEqual(connections.model.baseURLText, "kyotoagent://first.example:7841?token=new-token")
    }

    func testSelectingTheConnectedServerKeepsTheLiveModel() async {
        let gate = goodGate()
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("https://first.example")
        let original = connections.model
        let calls = gate.paths
        connections.showingServers = true
        await connections.connect("https://first.example")
        XCTAssertTrue(connections.model === original)
        XCTAssertEqual(gate.paths, calls)
        XCTAssertFalse(connections.showingServers)
    }

    func testFailedSecureSaveDoesNotReplaceTheCurrentModel() async {
        let connections = makeConnections(store: FailingServerStore())
        let original = connections.model
        await connections.connect("https://first.example")
        XCTAssertTrue(connections.model === original)
        XCTAssertTrue(connections.servers.isEmpty)
        XCTAssertNotNil(connections.failure)
    }

    func testRenameSurvivesRestartAndReconnectionWithoutChangingIdentity() async throws {
        let store = MemoryBaseURL()
        let connections = makeConnections(store: store)
        await connections.connect("https://first.example")
        let id = try XCTUnwrap(connections.activeID)
        let model = connections.model
        connections.rename(id, name: "  Work Mac  ")
        XCTAssertTrue(connections.model === model)
        let restored = makeConnections(store: store)
        XCTAssertEqual(restored.servers.first?.title, "Work Mac")
        await restored.connect("https://first.example")
        XCTAssertEqual(restored.activeID, id)
        XCTAssertEqual(restored.servers.first?.title, "Work Mac")
        restored.rename(id, name: "  ")
        XCTAssertEqual(restored.servers.first?.title, "https://first.example")
    }

    func testRemovingServersPreservesOtherConnectionAndDoesNotReimportLegacy() async throws {
        let store = MemoryBaseURL()
        let legacy = MemoryBaseURL(value: "https://first.example")
        let connections = makeConnections(store: store, legacy: legacy)
        await connections.connect("https://second.example")
        let active = try XCTUnwrap(connections.activeID)
        let model = connections.model
        connections.remove("legacy")
        XCTAssertTrue(connections.model === model)
        XCTAssertEqual(connections.activeID, active)
        connections.remove(active)
        XCTAssertFalse(connections.model.connected)
        XCTAssertNil(connections.activeID)
        XCTAssertEqual(connections.model.baseURLText, "")
        XCTAssertEqual(connections.address, "")
        let restored = makeConnections(store: store, legacy: legacy)
        XCTAssertTrue(restored.servers.isEmpty)
        XCTAssertNil(restored.activeID)
    }

    func testExchangedCredentialIsSavedInServerCatalogEvenWhenSessionsFail() async throws {
        for sessionsFail in [false, true] {
            let store = MemoryBaseURL()
            let gate = goodGate()
            gate.pairHandler = { request in try pairingFixture(request, accessToken: "issued-jwt") }
            if sessionsFail {
                gate.handler = { _ in HostResponse(status: 503, body: Data("Unavailable".utf8)) }
            }
            let connections = makeConnections(store: store, gate: gate)
            await connections.connect("kyotoagent://first.example:7841?token=pair_temporary")
            let restored = makeConnections(store: store, gate: goodGate())
            let server = try XCTUnwrap(restored.servers.first)
            XCTAssertEqual(server.connection, "kyotoagent://first.example:7841?token=issued-jwt")
            let cached = try XCTUnwrap(connections.models[server.id])
            if sessionsFail {
                gate.handler = goodGate().handler
                await connections.connect(server.connection)
                XCTAssertTrue(connections.model === cached)
                XCTAssertTrue(connections.model.connected)
            }
            XCTAssertFalse(try XCTUnwrap(store.value).contains("pair_temporary"))
            await restored.connect(server.connection)
            XCTAssertTrue(restored.model.connected)
        }
    }

    func testFailedRenameAndRemoveKeepSavedServerAndActiveModel() async throws {
        let connections = makeConnections(store: FailingServerStore(), legacy: MemoryBaseURL(value: "https://first.example"))
        let model = connections.model
        connections.rename("legacy", name: "Work")
        XCTAssertNil(connections.servers.first?.name)
        XCTAssertNotNil(connections.failure)
        connections.remove("legacy")
        XCTAssertEqual(connections.servers.count, 1)
        XCTAssertEqual(connections.activeID, "legacy")
        XCTAssertTrue(connections.model === model)
    }

    func testLaunchConnectsToLastSuccessfulServerOnlyOnce() async throws {
        let store = MemoryBaseURL()
        let connections = makeConnections(store: store)
        await connections.connect("https://first.example")
        await connections.connect("https://second.example")
        let gate = goodGate()
        let restored = makeConnections(store: store, gate: gate)
        await restored.connectOnLaunch()
        XCTAssertTrue(restored.model.connected)
        XCTAssertEqual(restored.model.baseURLText, "https://second.example")
        let calls = gate.paths
        let model = restored.model
        await restored.connectOnLaunch()
        XCTAssertTrue(restored.model === model)
        XCTAssertEqual(gate.paths, calls)
    }

    func testLaunchFailureKeepsSavedServerWithoutRepeatedAttempts() async throws {
        let store = MemoryBaseURL()
        let connections = makeConnections(store: store)
        await connections.connect("https://first.example")
        let gate = goodGate()
        gate.pairHandler = { _ in HostResponse(status: 401, body: Data("Pair again".utf8)) }
        let restored = makeConnections(store: store, gate: gate)
        await restored.connectOnLaunch()
        XCTAssertFalse(restored.model.connected)
        XCTAssertNotNil(restored.failure)
        XCTAssertEqual(restored.activeID, connections.activeID)
        XCTAssertEqual(restored.servers, connections.servers)
        let calls = gate.paths
        await restored.connectOnLaunch()
        XCTAssertEqual(gate.paths, calls)
    }

    func testLaunchWithoutSavedServerDoesNotConnect() async {
        let gate = goodGate()
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connectOnLaunch()
        XCTAssertTrue(gate.paths.isEmpty)
        XCTAssertFalse(connections.model.connected)
        XCTAssertNil(connections.failure)
    }

    func testLaunchConnectsOtherSavedServersWhenTheLastServerIsUnavailable() async throws {
        let store = MemoryBaseURL()
        let connections = makeConnections(store: store)
        await connections.connect("https://first.example")
        await connections.connect("https://second.example")
        let gate = goodGate()
        gate.pairHandler = { request in
            if request.url?.host == "second.example" {
                return HostResponse(status: 503, body: Data("Offline".utf8))
            }
            return try pairingFixture(request)
        }
        let restored = makeConnections(store: store, gate: gate)
        await restored.connectOnLaunch()
        XCTAssertTrue(restored.model.connected)
        XCTAssertEqual(restored.model.baseURLText, "https://first.example")
        XCTAssertEqual(restored.servers.count, 2)
    }

    func testAllSavedServersShareTheListWithDistinctIdentityAndOwnedSessionCreation() async throws {
        let gate = goodGate()
        let data = try pairingTestData("sessions.json")
        var sessions = try JSONDecoder().decode([Session].self, from: data)
        for index in sessions.indices { sessions[index].project = "shared" }
        let listed = try JSONEncoder().encode(sessions)
        gate.handler = { request in
            let host = request.url?.host ?? ""
            if request.httpMethod == "POST" {
                XCTAssertEqual(host, "second.example")
                let body = try XCTUnwrap(request.httpBody)
                let object = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
                XCTAssertEqual(object["workspace"] as? String, "/srv/second.example/shared")
                return HostResponse(status: 201, body: Data(#"{"id":"new-session","workspace":"/srv/second.example/shared","status":"idle"}"#.utf8))
            }
            switch request.url?.path {
            case "/v1/projects":
                return HostResponse(status: 200, body: Data("[{\"id\":\"shared\",\"name\":\"Shared\",\"path\":\"/srv/\(host)/shared\"}]".utf8))
            case "/v1/profiles":
                return HostResponse(status: 200, body: Data("[]".utf8))
            default:
                return HostResponse(status: 200, body: listed)
            }
        }
        let store = MemoryBaseURL()
        let saved = makeConnections(store: store, gate: gate)
        await saved.connect("https://first.example")
        await saved.connect("https://second.example")
        let connections = makeConnections(store: store, gate: gate)
        await connections.connectOnLaunch()
        XCTAssertEqual(connections.models.values.filter { $0.connected }.count, 2)
        XCTAssertEqual(connections.projects.count, 2)
        XCTAssertEqual(Set(connections.projects.map(\.id)).count, 2)
        XCTAssertEqual(connections.projectGroups.count, 2)
        let first = try XCTUnwrap(connections.servers.first { $0.address == "https://first.example" })
        let second = try XCTUnwrap(connections.servers.first { $0.address == "https://second.example" })
        let id = try XCTUnwrap(sessions.first?.id)
        connections.openSession(serverID: first.id, sessionID: id)
        connections.model.updateDraft("First draft")
        connections.openSession(serverID: second.id, sessionID: id)
        XCTAssertEqual(connections.model.draft, "")
        connections.model.updateDraft("Second draft")
        connections.openSession(serverID: first.id, sessionID: id)
        XCTAssertEqual(connections.model.draft, "First draft")
        let target = try XCTUnwrap(connections.models[second.id])
        target.useWorktree = false
        let created = await target.createSession()
        XCTAssertTrue(created)
        connections.selectServer(second.id)
        XCTAssertTrue(connections.model === target)
        XCTAssertEqual(connections.model.selection, "new-session")
    }

    func testPollingRecoveryPreservesTheSelectedServerAndItsDraft() async throws {
        let gate = goodGate()
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("https://first.example")
        let firstID = try XCTUnwrap(connections.activeID)
        let first = connections.model
        await connections.connect("https://second.example")
        let secondID = try XCTUnwrap(connections.activeID)
        let selected = connections.model
        selected.open(try XCTUnwrap(selected.sessions.first?.id))
        selected.updateDraft("Keep my selected server")
        gate.pairHandler = { _ in HostResponse(status: 503, body: Data("Offline".utf8)) }
        await first.connect()
        await selected.connect()
        gate.pairHandler = { request in
            if request.url?.host == "first.example" { return try pairingFixture(request) }
            return HostResponse(status: 503, body: Data("Offline".utf8))
        }
        let polling = Swift.Task { await connections.pollSessions() }
        for _ in 0..<100 where !first.connected {
            try await Swift.Task.sleep(for: .milliseconds(10))
        }
        polling.cancel()
        await polling.value
        XCTAssertTrue(connections.models[firstID]?.connected == true)
        XCTAssertEqual(connections.activeID, secondID)
        XCTAssertTrue(connections.model === selected)
        XCTAssertEqual(connections.model.draft, "Keep my selected server")
    }

    func testOfflineSessionSelectionKeepsTheCurrentServerAndSelection() async throws {
        let gate = goodGate()
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("https://first.example")
        let first = connections.model
        await connections.connect("https://second.example")
        let selected = connections.model
        let selectedID = try XCTUnwrap(connections.activeID)
        let sessionID = try XCTUnwrap(selected.sessions.first?.id)
        gate.pairHandler = { _ in HostResponse(status: 503, body: Data("Offline".utf8)) }
        await first.connect()
        await selected.connect()
        for server in connections.servers {
            connections.openSession(serverID: server.id, sessionID: sessionID)
            XCTAssertEqual(connections.activeID, selectedID)
            XCTAssertTrue(connections.model === selected)
            XCTAssertNil(connections.model.selection)
        }
    }

    func testLaunchAndPollingShareEachServerConnectionAttempt() async throws {
        let store = MemoryBaseURL()
        let gate = goodGate()
        let saved = makeConnections(store: store, gate: gate)
        await saved.connect("https://first.example")
        await saved.connect("https://second.example")
        let delayed = DelayedSessionResponse(gate: gate, suffix: "/v1/pair")
        let connections = makeConnections(store: store, gate: gate, transport: delayed)
        let launch = Swift.Task { await connections.connectOnLaunch() }
        await delayed.waitForRequests(2)
        let polling = Swift.Task { await connections.pollSessions() }
        try await Swift.Task.sleep(for: .milliseconds(100))
        let requests = await delayed.requests
        XCTAssertEqual(requests, 2)
        polling.cancel()
        var request = URLRequest(url: URL(string: "https://first.example/v1/pair")!)
        request.httpBody = try JSONEncoder().encode(PairingClient.ios)
        let response = try pairingFixture(request)
        for _ in 0..<requests { await delayed.finishNext(response) }
        await launch.value
        await polling.value
        XCTAssertEqual(connections.models.values.filter { $0.connected }.count, 2)
        XCTAssertEqual(connections.model.baseURLText, "https://second.example")
    }

    func testRefreshingAllProjectsPreservesEachServersCreationChoices() async throws {
        let gate = goodGate()
        gate.handler = { request in
            switch request.url?.path {
            case "/v1/projects":
                return HostResponse(status: 200, body: Data(#"[{"id":"first","name":"First","path":"/srv/first"},{"id":"second","name":"Second","path":"/srv/second"}]"#.utf8))
            case "/v1/profiles":
                return HostResponse(status: 200, body: Data(#"["review"]"#.utf8))
            default:
                return HostResponse(status: 200, body: try pairingTestData("sessions.json"))
            }
        }
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("https://first.example")
        await connections.connect("https://second.example")
        for candidate in connections.models.values {
            candidate.workspaceDraft = "/srv/my-work"
            candidate.useWorktree = false
            candidate.selectedProfile = "review"
            candidate.selectedProjectID = "second"
        }
        await connections.loadProjects()
        for candidate in connections.models.values {
            XCTAssertEqual(candidate.workspaceDraft, "/srv/my-work")
            XCTAssertFalse(candidate.useWorktree)
            XCTAssertEqual(candidate.selectedProfile, "review")
            XCTAssertEqual(candidate.selectedProjectID, "second")
        }
        let handler = gate.handler
        gate.handler = { _ in HostResponse(status: 503, body: Data("Offline".utf8)) }
        await connections.loadProjects()
        for candidate in connections.models.values {
            XCTAssertEqual(candidate.selectedProjectID, "second")
            XCTAssertNotNil(candidate.projectFailure)
        }
        gate.handler = handler
        await connections.loadProjects()
        for candidate in connections.models.values {
            XCTAssertEqual(candidate.selectedProjectID, "second")
            XCTAssertNil(candidate.projectFailure)
        }
    }

    func testPollingUsesTheReplacementModelAfterCredentialsChange() async throws {
        let gate = goodGate()
        let delayed = DelayedSessionResponse(gate: gate, suffix: "/v1/sessions")
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate, transport: delayed)
        let response = HostResponse(status: 200, body: try pairingTestData("sessions.json"))
        let initial = Swift.Task { await connections.connect("kyotoagent://first.example:7841?token=old-token") }
        await delayed.waitForRequests(1)
        await delayed.finishNext(response)
        await initial.value
        let original = connections.model
        let polling = Swift.Task { await connections.pollSessions() }
        await delayed.waitForRequests(2)
        let replacement = Swift.Task { await connections.connect("kyotoagent://first.example:7841?token=new-token") }
        await delayed.waitForRequests(3)
        await delayed.finishNext(response)
        await delayed.finishNext(response)
        await replacement.value
        XCTAssertFalse(connections.model === original)
        XCTAssertEqual(connections.servers.count, 1)
        await delayed.waitForRequests(4)
        let authorization = await delayed.authorizations.last ?? nil
        XCTAssertEqual(authorization, "Bearer new-token")
        polling.cancel()
        await delayed.finishNext(response)
        await polling.value
    }

    func testRenewedCredentialsRetryWithoutDiscardingTheCurrentDraft() async throws {
        let gate = goodGate()
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("kyotoagent://first.example:7841?token=old-token")
        let serverID = try XCTUnwrap(connections.activeID)
        let original = connections.model
        let sessionID = try XCTUnwrap(original.sessions.first?.id)
        original.open(sessionID)
        original.updateDraft("Keep the current draft")
        gate.pairHandler = { request in try pairingFixture(request, accessToken: "renewed-token") }
        gate.handler = { _ in HostResponse(status: 503, body: Data("Offline".utf8)) }
        await connections.connect("kyotoagent://first.example:7841?token=new-token")
        XCTAssertTrue(connections.model === original)
        XCTAssertEqual(connections.model.draft, "Keep the current draft")
        let recovering = try XCTUnwrap(connections.models[serverID])
        XCTAssertFalse(recovering === original)
        gate.handler = goodGate().handler
        let polling = Swift.Task { await connections.pollSessions() }
        let renewed = "kyotoagent://first.example:7841?token=renewed-token"
        for _ in 0..<100 where connections.model.baseURLText != renewed {
            try await Swift.Task.sleep(for: .milliseconds(10))
        }
        polling.cancel()
        await polling.value
        XCTAssertEqual(connections.activeID, serverID)
        XCTAssertTrue(connections.model === recovering)
        XCTAssertEqual(connections.model.baseURLText, renewed)
        XCTAssertEqual(connections.model.selection, sessionID)
        XCTAssertEqual(connections.model.draft, "Keep the current draft")
    }

    private func makeConnections(
        store: any BaseURLStoring,
        legacy: any BaseURLStoring = MemoryBaseURL(),
        gate: Gate? = nil,
        transport: (any HostTransport)? = nil
    ) -> ServerConnections {
        let gate = gate ?? goodGate()
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        return ServerConnections(store: store, legacy: legacy) { server in
            let directory = root.appendingPathComponent(server.id)
            return AppModel(
                store: MemoryBaseURL(value: server.connection),
                drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
                views: ViewCache(directory: directory.appendingPathComponent("views")),
                preferences: LastSessionPreference(defaults: UserDefaults(suiteName: root.lastPathComponent + server.id)!),
                transport: { transport ?? gate }
            )
        }
    }

    private func goodGate() -> Gate {
        let gate = Gate()
        gate.handler = { request in
            let file = request.url?.path == "/v1/sessions" ? "sessions.json" : "view.json"
            return HostResponse(status: 200, body: try pairingTestData(file))
        }
        return gate
    }
}

private struct FailingServerStore: BaseURLStoring {
    enum Failure: Error { case unavailable }
    func load() throws -> String? { nil }
    func save(_ url: String) throws { throw Failure.unavailable }
}
