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
        XCTAssertEqual(connections.servers.first?.id, SavedServer.legacyID)
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
        connections.remove(SavedServer.legacyID)
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
        connections.rename(SavedServer.legacyID, name: "Work")
        XCTAssertNil(connections.servers.first?.name)
        XCTAssertNotNil(connections.failure)
        connections.remove(SavedServer.legacyID)
        XCTAssertEqual(connections.servers.count, 1)
        XCTAssertEqual(connections.activeID, SavedServer.legacyID)
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

    func testLegacyServerIDMigratesToUUIDWithoutLosingSavedConnection() throws {
        let store = MemoryBaseURL(value: "{\"servers\":[{\"id\":\"legacy\",\"connection\":\"https://first.example\"}],\"activeID\":\"legacy\"}")
        let connections = makeConnections(store: store)
        XCTAssertEqual(connections.activeID, SavedServer.legacyID)
        XCTAssertNotNil(UUID(uuidString: try XCTUnwrap(connections.servers.first?.id)))
        XCTAssertEqual(connections.servers.first?.connection, "https://first.example")
        XCTAssertEqual(makeConnections(store: store).activeID, SavedServer.legacyID)
    }

    func testNotificationColdStartRoutesToSavedServerAndFetchesView() async throws {
        let store = MemoryBaseURL()
        let saved = makeConnections(store: store)
        await saved.connect("https://first.example")
        let firstID = try XCTUnwrap(saved.activeID)
        await saved.connect("https://second.example")
        let gate = goodGate()
        let restored = makeConnections(store: store, gate: gate)
        let route = try XCTUnwrap(NotificationRoute(payload: ["serverId": firstID, "sessionId": "a11a0001", "url": "https://attacker.example"]))
        let opened = await restored.openNotification(route)
        XCTAssertTrue(opened)
        XCTAssertEqual(restored.activeID, firstID)
        XCTAssertEqual(restored.model.selection, "a11a0001")
        XCTAssertNotNil(restored.model.transcript.view)
        XCTAssertTrue(gate.paths.contains("/v1/sessions/a11a0001/view"))
        let missing = try XCTUnwrap(NotificationRoute(payload: ["serverId": "removed", "sessionId": "a11a0001"]))
        let rejected = await restored.openNotification(missing)
        XCTAssertFalse(rejected)
        XCTAssertEqual(restored.activeID, firstID)
        let deleted = try XCTUnwrap(NotificationRoute(payload: ["serverId": firstID, "sessionId": "deleted"]))
        let deletedOpened = await restored.openNotification(deleted)
        XCTAssertFalse(deletedOpened)
        XCTAssertEqual(restored.model.selection, "a11a0001")
    }

    func testBatchDeletionUsesServerIdentityKeepsWorktreesAndRetainsOnlyFailuresForRetry() async throws {
        let gate = goodGate()
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("https://first.example")
        let firstID = try XCTUnwrap(connections.activeID)
        let first = connections.model
        await connections.connect("https://second.example")
        let secondID = try XCTUnwrap(connections.activeID)
        let second = connections.model
        first.open("c0ffee01")
        first.updateDraft("First draft")
        second.open("c0ffee01")
        second.updateDraft("Second draft")
        let firstKey = ServerItemID(serverID: firstID, itemID: "c0ffee01")
        let secondKey = ServerItemID(serverID: secondID, itemID: "c0ffee01")
        connections.beginSessionSelection()
        connections.toggleSessionSelection(firstKey)
        connections.toggleSessionSelection(secondKey)
        XCTAssertEqual(connections.selectedSessions.count, 2)
        connections.requestSelectedSessionDeletion()
        let snapshot = try XCTUnwrap(connections.sessionDeletion)
        XCTAssertEqual(snapshot.targets.count, 2)
        XCTAssertTrue(snapshot.message.contains("Worktrees and files will be kept"))
        connections.toggleSessionSelection(firstKey)
        XCTAssertEqual(connections.selectedSessions.count, 2)
        XCTAssertFalse(gate.calls.contains { $0.method == "DELETE" })
        let responses = BatchResponses()
        let rows = try JSONDecoder().decode([Session].self, from: pairingTestData("sessions.json"))
        gate.handler = { request in
            let host = request.url?.host ?? ""
            if request.httpMethod == "DELETE" {
                XCTAssertEqual(request.url?.path, "/v1/sessions/c0ffee01")
                XCTAssertNil(request.url?.query)
                if host == "second.example" { return HostResponse(status: 503, body: Data("Try later".utf8)) }
                responses.deleted.append(host)
                return HostResponse(status: 204, body: Data())
            }
            responses.refreshed.append(host)
            let listed = rows.filter { !(responses.deleted.contains(host) && $0.id == "c0ffee01") }
            return HostResponse(status: 200, body: try JSONEncoder().encode(listed))
        }
        await connections.confirmSessionDeletion(snapshot)
        XCTAssertEqual(responses.refreshed.sorted(), ["first.example", "second.example"])
        XCTAssertNil(first.selection)
        XCTAssertEqual(first.draft, "")
        XCTAssertNil(first.transcript.view)
        XCTAssertEqual(second.selection, "c0ffee01")
        XCTAssertEqual(second.draft, "Second draft")
        XCTAssertEqual(connections.activeID, secondID)
        XCTAssertEqual(connections.selectedSessions, [secondKey])
        XCTAssertEqual(connections.sessionDeletionFailures.map(\.message), ["Try later"])
        XCTAssertTrue(connections.selectingSessions)
        XCTAssertFalse(connections.deletingSessions)
        connections.requestSelectedSessionDeletion()
        let retry = try XCTUnwrap(connections.sessionDeletion)
        XCTAssertEqual(retry.targets.map(\.id), [secondKey])
        gate.handler = { request in
            if request.httpMethod == "DELETE" {
                XCTAssertEqual(request.url?.host, "second.example")
                XCTAssertNil(request.url?.query)
                return HostResponse(status: 204, body: Data())
            }
            return HostResponse(status: 200, body: Data("[]".utf8))
        }
        await connections.confirmSessionDeletion(retry)
        XCTAssertNil(second.selection)
        XCTAssertFalse(connections.selectingSessions)
        XCTAssertTrue(connections.sessionDeletionFailures.isEmpty)
        XCTAssertTrue(connections.selectedSessions.isEmpty)
    }

    func testBatchReportsEveryFailureAndDoesNotDiscardUnrelatedActiveChat() async throws {
        let gate = goodGate()
        var rows = try JSONDecoder().decode([Session].self, from: pairingTestData("sessions.json"))
        var extra = rows[0]
        extra.id = "deadbeef"
        rows.append(extra)
        let listed = try JSONEncoder().encode(rows)
        gate.handler = { _ in HostResponse(status: 200, body: listed) }
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("https://first.example")
        let serverID = try XCTUnwrap(connections.activeID)
        connections.model.open("a11a0001")
        connections.model.updateDraft("Keep the active chat")
        let keys = connections.model.sessions.filter { $0.id != "a11a0001" }.map {
            ServerItemID(serverID: serverID, itemID: $0.id)
        }
        XCTAssertGreaterThan(keys.count, 1)
        connections.beginSessionSelection()
        connections.updateSelectedSessions(Set(keys))
        connections.requestSelectedSessionDeletion()
        let snapshot = try XCTUnwrap(connections.sessionDeletion)
        gate.handler = { request in
            if request.httpMethod == "DELETE" {
                return HostResponse(status: 409, body: Data((request.url?.lastPathComponent ?? "").utf8))
            }
            return HostResponse(status: 200, body: listed)
        }
        await connections.confirmSessionDeletion(snapshot)
        XCTAssertEqual(connections.sessionDeletionFailures.count, keys.count)
        XCTAssertEqual(Set(connections.sessionDeletionFailures.map(\.id)), Set(keys))
        XCTAssertEqual(connections.selectedSessions, Set(keys))
        XCTAssertEqual(connections.model.selection, "a11a0001")
        XCTAssertEqual(connections.model.draft, "Keep the active chat")
    }

    func testArchivedDeletionSnapshotsCurrentServerDespiteSwitchAndListUpdates() async throws {
        let gate = goodGate()
        var rows = try JSONDecoder().decode([Session].self, from: pairingTestData("sessions.json"))
        for index in rows.indices { rows[index].archived = index == 0 }
        let listed = try JSONEncoder().encode(rows)
        gate.handler = { _ in HostResponse(status: 200, body: listed) }
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("https://first.example")
        let firstID = try XCTUnwrap(connections.activeID)
        await connections.connect("https://second.example")
        let secondID = try XCTUnwrap(connections.activeID)
        XCTAssertEqual(connections.currentServerArchivedCount, 1)
        connections.requestArchivedSessionDeletion()
        let snapshot = try XCTUnwrap(connections.sessionDeletion)
        XCTAssertEqual(snapshot.targets.map(\.id), [ServerItemID(serverID: secondID, itemID: rows[0].id)])
        XCTAssertEqual(snapshot.serverTitle, "https://second.example")
        connections.selectServer(firstID)
        for index in rows.indices { rows[index].archived = true }
        let updated = try JSONEncoder().encode(rows)
        gate.handler = { _ in HostResponse(status: 200, body: updated) }
        await connections.models[secondID]?.refreshSessions()
        gate.handler = { request in
            XCTAssertEqual(request.url?.host, "second.example")
            if request.httpMethod == "DELETE" {
                XCTAssertEqual(request.url?.path, "/v1/sessions/" + snapshot.targets[0].id.itemID!)
                XCTAssertNil(request.url?.query)
                return HostResponse(status: 204, body: Data())
            }
            return HostResponse(status: 200, body: Data("[]".utf8))
        }
        await connections.confirmSessionDeletion(snapshot)
        XCTAssertEqual(gate.calls.filter { $0.method == "DELETE" }.count, 1)
        XCTAssertEqual(connections.activeID, firstID)
    }

    func testBatchCancellationAndBusyStatePreventSelectionChangesAndDuplicateRequests() async throws {
        let gate = goodGate()
        let delayed = DelayedSessionResponse(gate: gate, suffix: "/c0ffee01")
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate, transport: delayed)
        await connections.connect("https://first.example")
        let key = ServerItemID(serverID: try XCTUnwrap(connections.activeID), itemID: "c0ffee01")
        connections.beginSessionSelection()
        connections.toggleSessionSelection(key)
        connections.requestSelectedSessionDeletion()
        connections.cancelSessionDeletion()
        XCTAssertNil(connections.sessionDeletion)
        XCTAssertEqual(connections.selectedSessions, [key])
        XCTAssertFalse(gate.calls.contains { $0.method == "DELETE" })
        connections.requestSelectedSessionDeletion()
        let snapshot = try XCTUnwrap(connections.sessionDeletion)
        let pending = Swift.Task { await connections.confirmSessionDeletion(snapshot) }
        await delayed.waitForRequests(1)
        XCTAssertTrue(connections.deletingSessions)
        connections.cancelSessionSelection()
        connections.toggleSessionSelection(key)
        XCTAssertEqual(connections.selectedSessions, [key])
        await connections.confirmSessionDeletion(snapshot)
        let count = await delayed.requests
        XCTAssertEqual(count, 1)
        await delayed.finishNext(HostResponse(status: 409, body: Data("Busy".utf8)))
        await pending.value
        XCTAssertFalse(connections.deletingSessions)
        XCTAssertEqual(connections.selectedSessions, [key])
        connections.cancelSessionSelection()
        XCTAssertTrue(connections.selectedSessions.isEmpty)
        XCTAssertFalse(connections.selectingSessions)
    }

    func testProjectRemovalRoutesToOwnerRefreshesRegistrationAndPreservesSessions() async throws {
        let gate = goodGate()
        let listed = try pairingTestData("sessions.json")
        let projects = Data(#"[{"id":"shared","name":"Shared","path":"/srv/shared"}]"#.utf8)
        gate.handler = { request in
            if request.url?.path == "/v1/projects" { return HostResponse(status: 200, body: projects) }
            if request.url?.path == "/v1/profiles" { return HostResponse(status: 200, body: Data("[]".utf8)) }
            return HostResponse(status: 200, body: listed)
        }
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("https://first.example")
        let first = connections.model
        let before = first.sessions
        first.open("a11a0001")
        first.updateDraft("Keep my draft")
        await connections.connect("https://second.example")
        let second = connections.model
        gate.handler = { request in
            XCTAssertEqual(request.url?.host, "first.example")
            if request.httpMethod == "DELETE" {
                XCTAssertEqual(request.url?.path, "/v1/projects/shared")
                return HostResponse(status: 204, body: Data())
            }
            return HostResponse(status: 200, body: Data("[]".utf8))
        }
        try await first.removeProject("shared")
        XCTAssertTrue(first.projects.isEmpty)
        XCTAssertNil(first.selectedProjectID)
        XCTAssertEqual(first.sessions, before)
        XCTAssertEqual(first.selection, "a11a0001")
        XCTAssertEqual(first.draft, "Keep my draft")
        XCTAssertEqual(second.projects.map(\.id), ["shared"])
        XCTAssertTrue(connections.model === second)
        XCTAssertFalse(gate.calls.contains { $0.method == "DELETE" && $0.path.hasPrefix("/v1/sessions/") })
    }

    func testProjectRemovalFailureRetainsRegistrationAndCanRetry() async throws {
        let gate = goodGate()
        let projects = Data(#"[{"id":"shared","name":"Shared","path":"/srv/shared"}]"#.utf8)
        gate.handler = { request in
            if request.url?.path == "/v1/projects" { return HostResponse(status: 200, body: projects) }
            if request.url?.path == "/v1/profiles" { return HostResponse(status: 200, body: Data("[]".utf8)) }
            return HostResponse(status: 200, body: try pairingTestData("sessions.json"))
        }
        let connections = makeConnections(store: MemoryBaseURL(), gate: gate)
        await connections.connect("https://first.example")
        gate.handler = { _ in HostResponse(status: 404, body: Data("Unknown project".utf8)) }
        do {
            try await connections.model.removeProject("shared")
            XCTFail("Expected a project removal error")
        } catch {
            XCTAssertEqual(error as? HostError, .status(404, "Unknown project"))
        }
        XCTAssertEqual(connections.model.projects.map(\.id), ["shared"])
        XCTAssertEqual(connections.model.selectedProjectID, "shared")
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

private final class BatchResponses: @unchecked Sendable {
    var deleted: [String] = []
    var refreshed: [String] = []
}
