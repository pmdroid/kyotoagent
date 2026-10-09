import XCTest
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
@testable import KyotoAgent

@MainActor
final class NotificationTests: XCTestCase {
    func testSessionAlertsUseNativeNotificationsWithoutCustomBanners() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let screen = try String(contentsOf: root.appendingPathComponent("App/ConnectScreen.swift"), encoding: .utf8)
        let panes = try String(contentsOf: root.appendingPathComponent("App/SessionPanes.swift"), encoding: .utf8)
        let coordinator = try String(contentsOf: root.appendingPathComponent("App/NotificationCoordinator.swift"), encoding: .utf8)
        XCTAssertFalse(screen.contains("SessionBanners"))
        XCTAssertFalse(panes.contains("session-banner"))
        XCTAssertTrue(coordinator.contains("[.banner, .list, .sound]"))
    }

    func testDevicePayloadPreservesVariableLengthTokenAndInstallationIdentity() throws {
        let device = DeviceRegistration(id: "installation", token: Data([0, 1, 15, 255, 128]), environment: .sandbox, serverId: "saved-server")
        let request = serveRequest(baseURL: URL(string: "https://trusted.example")!, call: .registerDevice(device))
        XCTAssertEqual(request.httpMethod, "PUT")
        XCTAssertEqual(request.url?.path, "/v1/devices")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
        let body = try JSONDecoder().decode(DeviceRegistration.self, from: XCTUnwrap(request.httpBody))
        XCTAssertEqual(body.token, "00010fff80")
        XCTAssertEqual(body.id, "installation")
        XCTAssertEqual(body.serverId, "saved-server")
        XCTAssertEqual(body.environment, .sandbox)
        let replacement = DeviceRegistration(id: body.id, token: Data(repeating: 255, count: 48), environment: .production, serverId: body.serverId)
        XCTAssertEqual(replacement.token.count, 96)
        XCTAssertEqual(replacement.id, body.id)
        XCTAssertNotEqual(replacement, body)
    }

    func testDeviceDeleteUsesJSONBody() throws {
        let request = serveRequest(baseURL: URL(string: "https://trusted.example")!, call: .deleteDevice("installation"))
        XCTAssertEqual(request.httpMethod, "DELETE")
        XCTAssertEqual(request.url?.path, "/v1/devices")
        let body = try JSONSerialization.jsonObject(with: XCTUnwrap(request.httpBody)) as? [String: String]
        XCTAssertEqual(body, ["id": "installation"])
    }

    func testAuthenticatedRegistrationAndServerFailure() async throws {
        let gate = Gate()
        gate.handler = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer saved-token")
            return HostResponse(status: 204, body: Data())
        }
        let client = ServeClient(baseURL: URL(string: "https://trusted.example")!, transport: gate, token: "saved-token")
        try await client.registerDevice(DeviceRegistration(id: "i", token: Data([1]), environment: .production, serverId: "s"))
        try await client.deleteDevice("i")
        gate.handler = { _ in HostResponse(status: 503, body: Data("offline".utf8)) }
        do {
            try await client.deleteDevice("i")
            XCTFail("Expected failure for retry")
        } catch { XCTAssertEqual(error as? HostError, .status(503, "offline")) }
    }

    func testRouteIgnoresURLAndSuppressesOnlyMatchingOpenSession() throws {
        let route = try XCTUnwrap(NotificationRoute(payload: ["serverId": "saved", "sessionId": "session-1", "url": "https://attacker.example"]))
        XCTAssertEqual(route.serverID, "saved")
        XCTAssertTrue(route.suppress(activeServerID: "saved", openSessionID: "session-1"))
        XCTAssertFalse(route.suppress(activeServerID: "other", openSessionID: "session-1"))
        XCTAssertFalse(route.suppress(activeServerID: "saved", openSessionID: "session-2"))
        XCTAssertFalse(route.suppress(activeServerID: "saved", openSessionID: nil))
        XCTAssertNil(NotificationRoute(payload: ["url": "https://attacker.example"]))
        XCTAssertNil(NotificationRoute(payload: ["serverId": "saved", "sessionId": "../sessions"]))
        XCTAssertNil(NotificationRoute(payload: ["serverId": "", "sessionId": "session-1"]))
    }

    func testEnvironmentReadsSignedEntitlementAndRejectsMissingOrTruncatedSignature() throws {
        func executable(_ environment: String) throws -> Data {
            let plist = try PropertyListSerialization.data(fromPropertyList: ["aps-environment": environment], format: .xml, options: 0)
            var data = Data()
            func word(_ value: UInt32, bigEndian: Bool = false) {
                let shifts = bigEndian ? [24, 16, 8, 0] : [0, 8, 16, 24]
                data.append(contentsOf: shifts.map { UInt8(truncatingIfNeeded: value >> $0) })
            }
            word(0xfeedfacf)
            for _ in 0..<3 { word(0) }
            word(1)
            word(16)
            word(0)
            word(0)
            word(0x1d)
            word(16)
            word(48)
            word(UInt32(28 + plist.count))
            word(0xfade0cc0, bigEndian: true)
            word(UInt32(28 + plist.count), bigEndian: true)
            word(1, bigEndian: true)
            word(5, bigEndian: true)
            word(20, bigEndian: true)
            word(0xfade7171, bigEndian: true)
            word(UInt32(8 + plist.count), bigEndian: true)
            data.append(plist)
            return data
        }
        let sandbox = try executable("development")
        XCTAssertEqual(PushEnvironment.signedExecutable(sandbox), .sandbox)
        XCTAssertEqual(PushEnvironment.signedExecutable(try executable("production")), .production)
        XCTAssertNil(PushEnvironment.signedExecutable(try executable("unknown")))
        XCTAssertNil(PushEnvironment.signedExecutable(Data()))
        for end in 0..<sandbox.count {
            XCTAssertNil(PushEnvironment.signedExecutable(Data(sandbox.prefix(end))))
        }
    }

    func testPermissionRequiresExplicitOptIn() {
        for permission in [NotificationPermission.notDetermined, .denied, .authorized] {
            XCTAssertFalse(permission.shouldRegister(enabled: false))
        }
        XCTAssertFalse(NotificationPermission.notDetermined.shouldRegister(enabled: true))
        XCTAssertFalse(NotificationPermission.denied.shouldRegister(enabled: true))
        XCTAssertTrue(NotificationPermission.authorized.shouldRegister(enabled: true))
    }
}
