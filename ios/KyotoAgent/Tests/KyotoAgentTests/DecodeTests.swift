import XCTest
@testable import KyotoAgent

final class DecodeTests: XCTestCase {
    func testVisualQuestionsPreserveSourceAndPreviewWithoutBreakingOldCards() throws {
        let old = Data(#"{"text":"Choose","choices":["A","B"],"eventId":"q1"}"#.utf8)
        let legacy = try JSONDecoder().decode(QuestionCard.self, from: old)
        XCTAssertNil(legacy.visuals)
        let json = Data(#"{"text":"Choose","choices":["A","B"],"eventId":"q2","visuals":[{"title":"Route A","alt":"Plan then build","source":"flowchart LR; A-->B","image":{"name":"diagram.png","mimeType":"image/png","data":"aGVsbG8="}}]}"#.utf8)
        let question = try JSONDecoder().decode(QuestionCard.self, from: json)
        XCTAssertEqual(question.visuals?.first?.title, "Route A")
        XCTAssertEqual(question.visuals?.first?.alt, "Plan then build")
        XCTAssertEqual(question.visuals?.first?.source, "flowchart LR; A-->B")
        let restored = try JSONDecoder().decode(QuestionCard.self, from: JSONEncoder().encode(question))
        XCTAssertEqual(restored, question)
        XCTAssertEqual(AnswerPayload(sheet: .question(question), choice: "A").id, "q2")
    }

    func testToolErrorActivitySurvivesViewCaching() throws {
        var view = try fixture(View.self, "view.json")
        view.status = .working
        view.phase = .thinking
        view.action = "attach_artifact failed (2 attempts): missing.jpg"
        view.retryStatus = "Provider busy"
        let cached = try JSONDecoder().decode(View.self, from: JSONEncoder().encode(view))
        XCTAssertEqual(cached.action, view.action)
        XCTAssertEqual(cached.retryStatus, view.retryStatus)
        XCTAssertEqual(cached.phase, .thinking)
        XCTAssertNil(try fixture(View.self, "view.json").action)
    }

    func testGoalStatusAndVerificationSurviveViewCaching() throws {
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: iosRoot().appendingPathComponent("Fixtures/view.json"))) as? [String: Any])
        object["goal"] = [
            "objective": "Create the report",
            "status": "complete",
            "token_budget": 10000,
            "tokens_used": 310,
            "rounds": 2,
            "verification": "Checked the saved report with a fresh command.",
            "evidence": [["tool": "run", "output": "exited 0", "args": ["argv": ["cat", "report.txt"]]]]
        ]
        let view = try JSONDecoder().decode(View.self, from: JSONSerialization.data(withJSONObject: object))
        let goal = try XCTUnwrap(view.goal)
        XCTAssertEqual(goal.objective, "Create the report")
        XCTAssertEqual(goal.status, .complete)
        XCTAssertEqual(goal.status.title, "Verified")
        XCTAssertEqual(goal.token_budget, 10000)
        XCTAssertEqual(goal.tokens_used, 310)
        XCTAssertEqual(goal.rounds, 2)
        XCTAssertTrue(goal.verification.contains("fresh command"))
        let cached = try JSONDecoder().decode(View.self, from: JSONEncoder().encode(view))
        XCTAssertEqual(cached.goal, goal)
        XCTAssertNil(try fixture(View.self, "view.json").goal)
    }

    func testSessionsDecode() throws {
        let sessions = try fixture([Session].self, "sessions.json")
        XCTAssertEqual(sessions.map(\.id), ["c0ffee01", "a11a0001"])
        XCTAssertEqual(sessions[0].status, .waiting)
        XCTAssertEqual(sessions[0].waiting, "permission")
        XCTAssertEqual(sessions[0].parentId, "a11a0001")
        XCTAssertEqual(sessions[0].isolation, "worktree")
        XCTAssertFalse(sessions[0].hidden)
        XCTAssertTrue(sessions[0].worktree)
        XCTAssertFalse(sessions[1].worktree)
        XCTAssertNotNil(sessions[0].pullUrl)
        XCTAssertTrue(sessions[0].compacting)
        XCTAssertTrue(sessions[0].yolo)
        XCTAssertFalse(sessions[0].enhance)
        XCTAssertTrue(sessions[0].showCloseout)
        XCTAssertTrue(sessions[1].enhance)
        XCTAssertTrue(sessions[1].showCloseout)
        XCTAssertEqual(sessions[0].allow.writePaths, ["/home/pascal/work/kyotoagent/src/view.rs"])
        XCTAssertEqual(sessions[0].allow.argv, [["cargo", "test", "--offline"]])
        XCTAssertNil(sessions[1].pullUrl)
        XCTAssertNil(sessions[1].parentId)
        XCTAssertNil(sessions[1].isolation)
        XCTAssertEqual(sessions[1].waiting, "question")
        try assertRoundTrip([Session].self, "sessions.json")
    }

    func testViewDecodesAndPermissionEventIdRoundTrips() throws {
        let view = try fixture(View.self, "view.json")
        XCTAssertEqual(view.status, .waiting)
        XCTAssertEqual(view.revision, 9)
        XCTAssertEqual(
            view.cards.map(\.kind),
            [.ask, .question, .question, .answer, .permission, .result, .proof, .enhance]
        )
        let enhance = try enhanceBody(view.cards.last!)
        XCTAssertEqual(enhance.eventId, "e-enhance")
        XCTAssertEqual(enhance.text, "Rewrite the permission card.")
        XCTAssertEqual(enhance.source, "Add eventId to the permission card.")
        XCTAssertEqual(enhance.model, "grok-4.7")
        XCTAssertNil(enhance.error)
        let openQuestion = try question(view.cards[1])
        XCTAssertEqual(openQuestion.eventId, "e-open")
        XCTAssertNil(openQuestion.answer)
        XCTAssertEqual(openQuestion.choices, ["Kyoto Agent", "Kyoto Agent CLI"])
        let settled = try question(view.cards[2])
        XCTAssertEqual(settled.eventId, "e-settled")
        XCTAssertEqual(settled.answer, "yes")
        let permissionCard = view.cards[4]
        let permission = try permissionBody(permissionCard)
        XCTAssertEqual(permission.eventId, "e-perm")
        XCTAssertNil(permission.decision)
        XCTAssertNil(permission.argv)
        XCTAssertEqual(permission.path, "/home/pascal/work/kyotoagent/src/view.rs")
        let encoded = try JSONEncoder().encode(permissionCard)
        let decoded = try JSONDecoder().decode(Card.self, from: encoded)
        XCTAssertEqual(try permissionBody(decoded).eventId, "e-perm")
        XCTAssertEqual(view.tasks.map(\.state), [.running])
        XCTAssertEqual(view.todos.first?.status, .in_progress)
        XCTAssertEqual(view.phase, .thinking)
        XCTAssertEqual(view.closeout.first?.status, .passed)
        XCTAssertEqual(view.context?.buckets.map(\.id), [.system, .messages])
        try assertRoundTrip(View.self, "view.json")
    }

    func testModelsDecode() throws {
        let models = try fixture([Model].self, "models.json")
        XCTAssertEqual(models[0].id, "grok-4.7")
        XCTAssertEqual(models[0].aliases, ["grok"])
        XCTAssertEqual(models[0].reasoning_efforts, ["low", "medium", "high"])
        XCTAssertEqual(models[0].context_length, 2_000_000)
        XCTAssertEqual(models[0].provider, "grok")
        XCTAssertEqual(models[1].id, "cursor/composer")
        XCTAssertTrue(models[1].aliases.isEmpty)
        XCTAssertTrue(models[1].reasoning_efforts.isEmpty)
        XCTAssertNil(models[1].context_length)
        XCTAssertNil(models[1].provider)
        try assertRoundTrip([Model].self, "models.json")
    }

    func testProjectsDecode() throws {
        let projects = try fixture([Project].self, "projects.json")
        XCTAssertEqual(projects.map(\.id), ["kyotoagent", "notes"])
        XCTAssertEqual(projects[0].yolo, true)
        XCTAssertNil(projects[1].yolo)
        try assertRoundTrip([Project].self, "projects.json")
    }

    func testFileDecodes() throws {
        let file = try fixture(File.self, "file.json")
        XCTAssertEqual(file.path, "src/view.rs")
        XCTAssertFalse(file.truncated)
        XCTAssertFalse(file.text.isEmpty)
        try assertRoundTrip(File.self, "file.json")
    }

    func testTaskDecodes() throws {
        let task = try fixture(Task.self, "task.json")
        XCTAssertEqual(task.id, "task01")
        XCTAssertEqual(task.argv, ["cargo", "test", "--offline"])
        XCTAssertEqual(task.state, .exited)
        XCTAssertEqual(task.exit, 0)
        XCTAssertFalse(task.tail?.isEmpty ?? true)
        try assertRoundTrip(Task.self, "task.json")
    }

    func testAcceptedBodiesDecode() throws {
        let started = try body(TurnAccepted.self, "turn.json")
        XCTAssertEqual(started.turnId, "t1")
        let queued = try body(AskQueued.self, "queued.json")
        XCTAssertTrue(queued.queued)
        let encoded = try JSONEncoder().encode(queued)
        let again = try JSONDecoder().decode(AskQueued.self, from: encoded)
        XCTAssertTrue(again.queued)
        XCTAssertThrowsError(try JSONDecoder().decode(AskQueued.self, from: Data(#"{"queued":false}"#.utf8)))
    }

    func testCardKindsOmitToolCalls() {
        XCTAssertEqual(
            Set(CardKind.allCases.map(\.rawValue)),
            ["ask", "question", "answer", "permission", "result", "proof", "artifact", "enhance"]
        )
        let toolCall = #"{"id":"c0","kind":"tool_call","at":"2026-10-01T21:00:00.000Z","body":{"text":"quiet"}}"#
        XCTAssertThrowsError(try JSONDecoder().decode(Card.self, from: Data(toolCall.utf8)))
        let hyphenated = #"{"id":"c0","kind":"tool-call","at":"2026-10-01T21:00:00.000Z","body":{"text":"quiet"}}"#
        XCTAssertThrowsError(try JSONDecoder().decode(Card.self, from: Data(hyphenated.utf8)))
    }

    func testPermissionWithoutEventIdFails() {
        let missing = #"{"id":"c5","kind":"permission","at":"2026-10-01T21:00:00.000Z","body":{"action":"Replace src/view.rs","argv":null,"decision":null,"path":"src/view.rs","timeoutSec":null}}"#
        XCTAssertThrowsError(try JSONDecoder().decode(Card.self, from: Data(missing.utf8)))
        let nullId = #"{"id":"c5","kind":"permission","at":"2026-10-01T21:00:00.000Z","body":{"action":"Replace src/view.rs","eventId":null}}"#
        XCTAssertThrowsError(try JSONDecoder().decode(Card.self, from: Data(nullId.utf8)))
    }

    func testAMissingEnhanceKeyIsFalseAndAMissingCloseoutKeyIsTrue() throws {
        var raw = try String(contentsOf: iosRoot().appendingPathComponent("Fixtures/sessions.json"), encoding: .utf8)
        raw = raw.replacingOccurrences(of: "\"enhance\": false,\n", with: "")
        raw = raw.replacingOccurrences(of: "\"enhance\": true,\n", with: "")
        let stripped = try JSONDecoder().decode([Session].self, from: Data(raw.utf8))
        XCTAssertFalse(stripped[0].enhance)
        XCTAssertFalse(stripped[1].enhance)
        XCTAssertTrue(stripped[0].showCloseout)
        XCTAssertTrue(stripped[1].showCloseout)
        let hidden = raw.replacingOccurrences(
            of: "\"yolo\": true,",
            with: "\"yolo\": true,\n    \"showCloseout\": false,"
        )
        let decoded = try JSONDecoder().decode([Session].self, from: Data(hidden.utf8))
        XCTAssertFalse(decoded[0].showCloseout)
        XCTAssertTrue(decoded[1].showCloseout)
        let shown = raw.replacingOccurrences(
            of: "\"yolo\": false,",
            with: "\"yolo\": false,\n    \"showCloseout\": true,"
        )
        let present = try JSONDecoder().decode([Session].self, from: Data(shown.utf8))
        XCTAssertTrue(present[1].showCloseout)
    }

    func testQuestionWithoutEventIdFails() {
        let missing = #"{"id":"c2","kind":"question","at":"2026-10-01T21:00:00.000Z","body":{"answer":null,"choices":[],"text":"Which title?"}}"#
        XCTAssertThrowsError(try JSONDecoder().decode(Card.self, from: Data(missing.utf8)))
    }

    func testProjectSetsBundleAndDeploymentTarget() throws {
        let project = try String(contentsOf: projectFile(), encoding: .utf8)
        XCTAssertTrue(project.contains("PRODUCT_BUNDLE_IDENTIFIER = sh.pascal.kyotoagent;"))
        XCTAssertTrue(project.contains("IPHONEOS_DEPLOYMENT_TARGET = 26.0;"))
        XCTAssertTrue(project.contains("INFOPLIST_KEY_CFBundleDisplayName = \"Kyoto Agent\";"))
        XCTAssertTrue(project.contains("PRODUCT_NAME = \"Kyoto Agent\";"))
        XCTAssertEqual(project.components(separatedBy: "isa = PBXNativeTarget;").count - 1, 1)
        XCTAssertFalse(project.contains("URLSession"))
        XCTAssertFalse(project.contains("exportArchive"))
        XCTAssertFalse(project.contains("altool"))
    }

    func testScreensStayInTheAppTarget() throws {
        let sources = packageRoot().appendingPathComponent("Sources")
        let sourceEnumerator = FileManager.default.enumerator(at: sources, includingPropertiesForKeys: nil)
        var sawSources = false
        while let url = sourceEnumerator?.nextObject() as? URL {
            guard url.pathExtension == "swift" else { continue }
            sawSources = true
            let text = try String(contentsOf: url, encoding: .utf8)
            XCTAssertFalse(text.contains("import SwiftUI"), url.path)
            XCTAssertFalse(text.contains("UIViewController"), url.path)
        }
        XCTAssertTrue(sawSources)
        let app = packageRoot().appendingPathComponent("App")
        let appEnumerator = FileManager.default.enumerator(at: app, includingPropertiesForKeys: nil)
        var sawApp = false
        while let url = appEnumerator?.nextObject() as? URL {
            guard url.pathExtension == "swift" else { continue }
            sawApp = true
            let text = try String(contentsOf: url, encoding: .utf8)
            if url.lastPathComponent != "PairingScanner.swift" {
                XCTAssertFalse(text.contains("UIViewController"), url.path)
            }
            XCTAssertFalse(text.replacingOccurrences(of: modelConfirmSentence, with: "").contains("config.toml"), url.path)
            XCTAssertFalse(text.contains("Bearer "), url.path)
            XCTAssertFalse(text.contains("setValue") && text.contains("Authorization"), url.path)
        }
        XCTAssertTrue(sawApp)
        let project = try String(contentsOf: projectFile(), encoding: .utf8)
        XCTAssertFalse(project.contains("NSAllowsArbitraryLoads"))
    }

    private func question(_ card: Card) throws -> QuestionCard {
        guard case .question(let body) = card.body else {
            XCTFail("expected a question")
            throw DecodeTestsError.notQuestion
        }
        return body
    }

    private func enhanceBody(_ card: Card) throws -> EnhanceCard {
        guard case .enhance(let body) = card.body else {
            XCTFail("expected an enhance card")
            throw DecodeTestsError.notPermission
        }
        return body
    }

    private func permissionBody(_ card: Card) throws -> PermissionCard {
        guard case .permission(let body) = card.body else {
            XCTFail("expected a permission")
            throw DecodeTestsError.notPermission
        }
        return body
    }

    private func assertRoundTrip<T: Codable & Equatable>(_ type: T.Type, _ name: String) throws {
        let value = try fixture(type, name)
        let encoded = try JSONEncoder().encode(value)
        let decoded = try JSONDecoder().decode(type, from: encoded)
        XCTAssertEqual(decoded, value)
    }

    private func fixture<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
        let url = iosRoot().appendingPathComponent("Fixtures").appendingPathComponent(name)
        return try JSONDecoder().decode(type, from: Data(contentsOf: url))
    }

    private func body<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("Bodies")
            .appendingPathComponent(name)
        return try JSONDecoder().decode(type, from: Data(contentsOf: url))
    }

    private func iosRoot() -> URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
    }

    private func packageRoot() -> URL {
        iosRoot().appendingPathComponent("KyotoAgent")
    }

    private func projectFile() -> URL {
        packageRoot()
            .appendingPathComponent("KyotoAgent.xcodeproj")
            .appendingPathComponent("project.pbxproj")
    }
}

private enum DecodeTestsError: Error {
    case notQuestion
    case notPermission
}
