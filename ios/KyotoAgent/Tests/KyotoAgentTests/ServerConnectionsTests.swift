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

    private func makeConnections(
        store: any BaseURLStoring,
        legacy: any BaseURLStoring = MemoryBaseURL(),
        gate: Gate? = nil
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
                transport: { gate }
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
