import XCTest
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
@testable import KyotoAgent

func pairingFixture(_ request: URLRequest, accessToken: String? = nil) throws -> HostResponse {
    let client = try JSONDecoder().decode(PairingClient.self, from: request.httpBody ?? Data())
    var response: [String: Any] = [
        "server": ["name": "Kyoto Agent", "version": "0.1.0", "workspace": "/remote/work", "model": "remote-model", "effort": "high", "yolo": false, "enhance": true, "show_closeout": true, "profile": "review"],
        "client": ["name": client.name, "version": client.version],
        "models": [["id": "remote-model", "context_length": 32000]],
        "repositories": [["id": "remote-repo", "name": "Remote repository", "path": "/remote/repo"]],
    ]
    if let accessToken {
        response["access_token"] = accessToken
    }
    return HostResponse(status: 200, body: try JSONSerialization.data(withJSONObject: response))
}

func pairingTestData(_ name: String) throws -> Data {
    let directory = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: directory.appendingPathComponent("Fixtures").appendingPathComponent(name))
}

@MainActor
final class PairingTests: XCTestCase {
    func testVerifiedExchangePrecedesCredentialSaveAndDoesNotSeedCatalogs() async throws {
        let gate = Gate()
        gate.pairHandler = { request in
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertEqual(request.url?.path, "/v1/pair")
            XCTAssertNil(request.url?.query)
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer pair_temporary-code")
            let identity = try JSONDecoder().decode(PairingClient.self, from: request.httpBody ?? Data())
            XCTAssertEqual(identity, .ios)
            return try pairingFixture(request, accessToken: "issued-jwt")
        }
        gate.handler = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer issued-jwt")
            switch request.url?.path {
            case "/v1/sessions":
                return HostResponse(status: 200, body: try pairingTestData("sessions.json"))
            case "/v1/models":
                return HostResponse(status: 200, body: Data("[{\"id\":\"fresh-model\"}]".utf8))
            case "/v1/projects":
                return HostResponse(status: 200, body: Data("[{\"id\":\"fresh-repo\",\"name\":\"Fresh repository\",\"path\":\"/fresh/repo\"}]".utf8))
            case "/v1/profiles":
                return HostResponse(status: 200, body: Data("[]".utf8))
            default:
                return HostResponse(status: 200, body: try pairingTestData("view.json"))
            }
        }
        let store = MemoryBaseURL()
        let model = makeModel(store: store, gate: gate)
        let uri = "kyotoagent://box.example:7841?token=pair_temporary-code"
        let savedURI = "kyotoagent://box.example:7841?token=issued-jwt"
        model.baseURLText = uri
        await model.connect()
        XCTAssertTrue(model.connected)
        XCTAssertEqual(Array(gate.paths.prefix(2)), ["/v1/pair", "/v1/sessions"])
        XCTAssertEqual(store.saves, [savedURI])
        XCTAssertEqual(model.baseURLText, savedURI)
        XCTAssertEqual(model.pairingConfirmation?.server.workspace, "/remote/work")
        XCTAssertEqual(model.pairingConfirmation?.models.map(\.id), ["remote-model"])
        XCTAssertTrue(model.models.isEmpty)
        XCTAssertTrue(model.projects.isEmpty)
        await model.openModelList()
        XCTAssertEqual(model.models.map(\.id), ["fresh-model"])
        await model.loadProjects()
        XCTAssertEqual(model.projects.map(\.id), ["fresh-repo"])
        model.dismissPairingConfirmation()
        XCTAssertNil(model.pairingConfirmation)
        gate.pairHandler = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer issued-jwt")
            return try pairingFixture(request, accessToken: "issued-jwt")
        }
        let reopened = makeModel(store: store, gate: gate)
        await reopened.connect()
        XCTAssertTrue(reopened.connected)
        XCTAssertEqual(store.saves, [savedURI, savedURI])
    }

    func testRejectedOrUnverifiedExchangeNeverSavesCredentials() async throws {
        for fault in ["denied", "malformed", "name", "version", "server-name", "server-version", "workspace", "missing-token", "empty-token", "control-token"] {
            let gate = Gate()
            gate.pairHandler = { request in
                if fault == "denied" {
                    return HostResponse(status: 401, body: Data("{\"error\":\"unauthorized\"}".utf8))
                }
                if fault == "malformed" {
                    return HostResponse(status: 200, body: Data("[]".utf8))
                }
                let valid = try pairingFixture(request, accessToken: "issued-jwt")
                var body = try JSONSerialization.jsonObject(with: valid.body) as! [String: Any]
                var client = body["client"] as! [String: Any]
                var server = body["server"] as! [String: Any]
                switch fault {
                case "name": client["name"] = "Different client"
                case "version": client["version"] = "different-version"
                case "server-name": server["name"] = " "
                case "server-version": server["version"] = ""
                case "workspace": server["workspace"] = "relative/path"
                case "missing-token": body.removeValue(forKey: "access_token")
                case "empty-token": body["access_token"] = ""
                case "control-token": body["access_token"] = "invalid\r\ntoken"
                default: XCTFail("Unexpected pairing fault")
                }
                body["client"] = client
                body["server"] = server
                return HostResponse(status: 200, body: try JSONSerialization.data(withJSONObject: body))
            }
            let store = MemoryBaseURL()
            let model = makeModel(store: store, gate: gate)
            model.baseURLText = "kyotoagent://box.example:7841?token=pair_temporary-code"
            await model.connect()
            XCTAssertFalse(model.connected, fault)
            XCTAssertTrue(store.saves.isEmpty, fault)
            XCTAssertNotNil(model.failure, fault)
            XCTAssertNil(model.pairingConfirmation, fault)
            XCTAssertEqual(gate.paths, ["/v1/pair"], fault)
        }
    }

    func testExchangedCredentialSurvivesSessionListFailureAndReopen() async throws {
        let gate = Gate()
        gate.pairHandler = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer pair_temporary-code")
            return try pairingFixture(request, accessToken: "issued-jwt")
        }
        gate.handler = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer issued-jwt")
            return HostResponse(status: 503, body: Data("unavailable".utf8))
        }
        let store = MemoryBaseURL()
        let model = makeModel(store: store, gate: gate)
        model.baseURLText = "kyotoagent://box.example:7841?token=pair_temporary-code"
        await model.connect()
        XCTAssertFalse(model.connected)
        XCTAssertEqual(store.value, "kyotoagent://box.example:7841?token=issued-jwt")
        gate.pairHandler = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer issued-jwt")
            return try pairingFixture(request, accessToken: "issued-jwt")
        }
        gate.handler = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer issued-jwt")
            return HostResponse(status: 200, body: try pairingTestData("sessions.json"))
        }
        let reopened = makeModel(store: store, gate: gate)
        await reopened.connect()
        XCTAssertTrue(reopened.connected)
    }

    private func makeModel(store: MemoryBaseURL, gate: Gate) -> AppModel {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        return AppModel(
            store: store,
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: ViewCache(directory: directory.appendingPathComponent("views")),
            preferences: LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!),
            transport: { gate }
        )
    }
}
