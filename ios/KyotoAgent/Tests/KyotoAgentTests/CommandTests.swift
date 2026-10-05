import XCTest
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
@testable import KyotoAgent

let modelConfirmSentence = "Save as default for all sessions"

final class CommandTests: XCTestCase {
    func testSlashClassificationMatchesTheTUIRules() {
        let efforts = ["low", "medium", "high"]
        XCTAssertEqual(slashCommand("/compact", efforts: efforts), .compact)
        XCTAssertEqual(slashCommand("  /compact  ", efforts: efforts), .compact)
        XCTAssertEqual(slashCommand("/compact now", efforts: efforts), .ask)
        XCTAssertEqual(slashCommand("/yolo", efforts: efforts), .yolo(.toggle))
        XCTAssertEqual(slashCommand("/yolo on", efforts: efforts), .yolo(.on))
        XCTAssertEqual(slashCommand("/yolo off", efforts: efforts), .yolo(.off))
        XCTAssertEqual(slashCommand("/yolo now", efforts: efforts), .ask)
        XCTAssertEqual(slashCommand("/yolo on extra", efforts: efforts), .ask)
        XCTAssertEqual(slashCommand("/model", efforts: efforts), .openModel)
        XCTAssertEqual(slashCommand("/model grok-4.7", efforts: efforts), .setModel("grok-4.7"))
        XCTAssertEqual(slashCommand("/effort", efforts: efforts), .openEffort)
        XCTAssertEqual(slashCommand("/effort high", efforts: efforts), .setEffort("high"))
        XCTAssertEqual(slashCommand("/effort xhigh", efforts: efforts), .ask)
        XCTAssertEqual(slashCommand("/effort now", efforts: efforts), .ask)
        XCTAssertEqual(slashCommand("/preflight", efforts: efforts), .ask)
        XCTAssertEqual(slashCommand("open http links", efforts: efforts), .ask)
        XCTAssertEqual(slashCommand("/ model", efforts: efforts), .ask)
    }

    func testCatalogOrderAndHelpUseTheSameLocalList() throws {
        let skills = [
            Skill(name: "hidden", description: "no", disable_model_invocation: true, user_invocable: false, path: "hidden"),
            Skill(name: "preflight", description: "Closeout for a change.", disable_model_invocation: false, user_invocable: true, path: "preflight"),
        ]
        let titles = commandCatalog(skills: skills).map(\.title)
        XCTAssertEqual(titles, [
            "New session",
            "Open model",
            "Effort",
            "Compact",
            "Yolo",
            "Profile",
            "Cancel",
            "Delete session",
            "Help",
            "/model",
            "/effort",
            "/compact",
            "/yolo",
            "/preflight",
        ])
        let listed = [
            "New session",
            "Open model",
            "Effort",
            "Compact",
            "Yolo",
            "Profile",
            "Cancel",
            "Delete session",
            "Help",
        ]
        XCTAssertEqual(helpCatalog(skills: skills).map(\.title), listed)
        XCTAssertEqual(filteredCatalog(skills: skills, query: "").map(\.title), listed)
        XCTAssertFalse(helpCatalog(skills: skills).contains { $0.title.hasPrefix("/") })
        XCTAssertFalse(filteredCatalog(skills: skills, query: "pre").contains { $0.title.hasPrefix("/") })
        XCTAssertEqual(filteredCatalog(skills: skills, query: "/y").map(\.title), ["Yolo"])
        XCTAssertEqual(profileChoices([" review ", "planning", "review", "Everything", ""]), ["review", "planning", "Everything"])
        XCTAssertNil(createProfileField(nil))
        XCTAssertNil(createProfileField("Everything"))
        XCTAssertNil(createProfileField("  "))
        XCTAssertEqual(createProfileField("review"), "review")
        XCTAssertEqual(liveProfileField("Everything"), "")
        XCTAssertEqual(liveProfileField("review"), "review")
        XCTAssertTrue(filteredCatalog(skills: skills, query: "/").isEmpty)
        XCTAssertEqual(skillMatches(skills, draft: "/").map(\.name), ["preflight"])
        XCTAssertEqual(skillMatches(skills, draft: "/pr").map(\.name), ["preflight"])
        XCTAssertTrue(skillMatches(skills, draft: filledSkill("preflight")).isEmpty)
        XCTAssertTrue(skillMatches(skills, draft: "/preflight check this").isEmpty)
        XCTAssertTrue(skillMatches(skills, draft: "preflight").isEmpty)
        XCTAssertEqual(filledSkill("preflight"), "/preflight ")
        let sheet = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("App")
            .appendingPathComponent("CommandSheets.swift")
        let text = try String(contentsOf: sheet, encoding: .utf8)
        XCTAssertTrue(text.contains(modelConfirmSentence))
        XCTAssertTrue(text.contains("Text(modelConfirmTitle)"))
    }

    func testModelPostBodyUsesTheFixtureEfforts() throws {
        let models = try JSONDecoder().decode([Model].self, from: commandFixture("models.json"))
        let kept = postedModel(models: models, modelID: "grok-4.7", effort: "medium")
        XCTAssertEqual(kept, ModelPayload(model: "grok-4.7", effort: "medium"))
        let dropped = postedModel(models: models, modelID: "grok-4.7", effort: "xhigh")
        XCTAssertEqual(dropped, ModelPayload(model: "grok-4.7", effort: nil))
        let empty = postedModel(models: models, modelID: "cursor/composer", effort: "low")
        XCTAssertNil(empty.effort)
        XCTAssertEqual(effortChoices(for: "grok-4.7", in: models), ["low", "medium", "high"])
        XCTAssertEqual(effortChoices(for: "cursor/composer", in: models), [])
        let body = try modelPostBody(model: kept.model, effort: kept.effort)
        let decoded = try JSONDecoder().decode(ModelPayload.self, from: body)
        XCTAssertEqual(decoded, kept)
        let cleared = try modelPostBody(model: "cursor/composer", effort: nil)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: cleared) as? [String: Any])
        XCTAssertEqual(object["model"] as? String, "cursor/composer")
        XCTAssertTrue(object["effort"] is NSNull)
    }
}

@MainActor
final class CommandClientTests: XCTestCase {
    func testBareCompactPostsCompactAndCompactNowPostsAMessage() async throws {
        let gate = try scriptGate()
        let model = try await open(gate)
        model.updateDraft("/compact")
        await model.send()
        XCTAssertEqual(model.draft, "")
        XCTAssertEqual(posts(gate), ["/v1/pair", "/v1/sessions/a11a0001/compact"])
        model.updateDraft("/compact now")
        await model.send()
        let message = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/messages") })
        let payload = try JSONDecoder().decode(MessageText.self, from: message.body ?? Data())
        XCTAssertEqual(payload.text, "/compact now")
    }

    func testYoloCommandsPostYoloAndYoloNowPostsAMessage() async throws {
        let gate = try scriptGate()
        let model = try await open(gate)
        await model.refreshOpenView()
        XCTAssertEqual(model.answerSheet?.isPermission, true)
        let before = gate.calls.count
        model.updateDraft("/yolo on")
        await model.send()
        let yolo = try XCTUnwrap(gate.calls.dropFirst(before).first { $0.path.hasSuffix("/yolo") })
        let flag = try JSONDecoder().decode(YoloBody.self, from: yolo.body ?? Data())
        XCTAssertTrue(flag.yolo)
        XCTAssertEqual(yolo.method, "POST")
        XCTAssertFalse(gate.calls.contains { $0.path.contains("/answers") })
        model.updateDraft("/yolo off")
        await model.send()
        let off = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/yolo") })
        XCTAssertFalse(try JSONDecoder().decode(YoloBody.self, from: off.body ?? Data()).yolo)
        model.updateDraft("/yolo")
        await model.send()
        let toggled = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/yolo") })
        XCTAssertTrue(try JSONDecoder().decode(YoloBody.self, from: toggled.body ?? Data()).yolo)
        model.updateDraft("/yolo now")
        await model.send()
        let message = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/messages") })
        XCTAssertEqual(try JSONDecoder().decode(MessageText.self, from: message.body ?? Data()).text, "/yolo now")
        XCTAssertFalse(gate.calls.contains { $0.path.contains("/answers") })
    }

    func testModelSelectionKeepsProviderAndItsEfforts() async throws {
        let gate = try scriptGate()
        let original = gate.handler
        gate.handler = { request in
            if request.url?.path == "/v1/models" {
                return HostResponse(status: 200, body: Data(#"[{"id":"shared","provider":"first","reasoning_efforts":["low"]},{"id":"shared","provider":"second","reasoning_efforts":["high"]}]"#.utf8))
            }
            return try original(request)
        }
        let model = try await open(gate)
        await model.openModelList()
        await model.saveModel("shared", effort: "high", provider: "second")
        let call = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/model/session") })
        XCTAssertEqual(
            try JSONDecoder().decode(ModelPayload.self, from: call.body ?? Data()),
            ModelPayload(model: "shared", effort: "high", provider: "second")
        )
        XCTAssertNil(model.overlay)
        gate.handler = { request in
            if request.url?.path.hasSuffix("/model/session") == true {
                return HostResponse(status: 400, body: Data("Provider unavailable".utf8))
            }
            return try original(request)
        }
        await model.openModelList()
        await model.saveModel("shared", effort: "high", provider: "second")
        XCTAssertEqual(model.overlay, .model)
        XCTAssertEqual(model.notice, "Provider unavailable")
        XCTAssertFalse(model.savingModel)
    }

    func testSavingDefaultsIsExplicitAndOldServersKeepThePickerOpen() async throws {
        let gate = try scriptGate()
        let original = gate.handler
        gate.handler = { request in
            if request.url?.path.hasSuffix("/model/session") == true {
                return HostResponse(status: 404, body: Data())
            }
            if request.url?.path.hasSuffix("/model") == true {
                return HostResponse(status: 204, body: Data())
            }
            return try original(request)
        }
        let model = try await open(gate)
        await model.openModelList()
        await model.saveModel("grok-4.7", effort: "high")
        XCTAssertEqual(model.overlay, .model)
        XCTAssertEqual(model.notice, "Update the server to switch models for this session.")
        XCTAssertFalse(gate.calls.contains { $0.path.hasSuffix("/model") })
        await model.saveModel("grok-4.7", effort: "high", saveAsDefault: true)
        XCTAssertEqual(gate.calls.filter { $0.path.hasSuffix("/model") }.count, 1)
        XCTAssertNil(model.overlay)
    }

    func testSuccessfulModelSwitchSurvivesFailedSessionRefresh() async throws {
        let gate = try scriptGate()
        let model = try await open(gate)
        await model.openModelList()
        let original = gate.handler
        gate.handler = { request in
            if request.url?.path == "/v1/sessions" {
                return HostResponse(status: 503, body: Data())
            }
            return try original(request)
        }
        await model.saveModel("grok-4.7", effort: "medium", provider: "grok", dismiss: false)
        let session = try XCTUnwrap(model.sessions.first { $0.id == model.selection })
        XCTAssertEqual(session.model, "grok-4.7")
        XCTAssertEqual(session.effort, "medium")
        XCTAssertEqual(session.provider, "grok")
        XCTAssertEqual(model.overlay, .model)
    }

    func testModelConfirmPostsTheFixturePair() async throws {
        let gate = try scriptGate()
        let model = try await open(gate)
        model.updateDraft("/model grok-4.7")
        await model.send()
        let kept = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/model/session") })
        XCTAssertEqual(kept.method, "POST")
        XCTAssertEqual(
            try JSONDecoder().decode(ModelPayload.self, from: kept.body ?? Data()),
            ModelPayload(model: "grok-4.7", effort: "high")
        )
        model.updateDraft("/model cursor/composer")
        await model.send()
        let cleared = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/model/session") })
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: cleared.body ?? Data()) as? [String: Any])
        XCTAssertEqual(object["model"] as? String, "cursor/composer")
        XCTAssertTrue(object["effort"] is NSNull)
        await model.openModelList()
        await model.saveModel("grok-4.7", effort: "xhigh")
        let dropped = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/model/session") })
        XCTAssertNil(try JSONDecoder().decode(ModelPayload.self, from: dropped.body ?? Data()).effort)
        await model.saveModel("grok-4.7", effort: "medium")
        let chosen = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/model/session") })
        XCTAssertEqual(
            try JSONDecoder().decode(ModelPayload.self, from: chosen.body ?? Data()).effort,
            "medium"
        )
        XCTAssertEqual(model.overlay, nil)
    }

    func testHelpAndPaletteActionsStayOnTheLocalCatalog() async throws {
        let gate = try scriptGate()
        let model = try await open(gate)
        await model.refreshOpenView()
        let before = gate.calls.count
        let entries = model.helpEntries()
        XCTAssertEqual(entries.map(\.title).contains("Help"), true)
        XCTAssertEqual(entries.map(\.title).contains("Yolo"), true)
        XCTAssertEqual(entries.map(\.title).contains("Compact"), true)
        XCTAssertFalse(entries.contains { $0.title.hasPrefix("/") })
        let help = await model.runPalette(.help)
        XCTAssertEqual(help, .showHelp)
        XCTAssertEqual(gate.calls.count, before)
        let newer = await model.runPalette(.newSession)
        XCTAssertEqual(newer, .showNewSession)
        XCTAssertTrue(model.presentNewSession)
        XCTAssertFalse(gate.calls.dropFirst(before).contains { $0.method == "POST" || $0.method == "DELETE" })
        await model.runPalette(.cancel)
        XCTAssertTrue(gate.calls.contains { $0.method == "POST" && $0.path == "/v1/sessions/a11a0001/cancel" })
        await model.runPalette(.closeSession)
        XCTAssertEqual(model.deleteConfirm?.id, "a11a0001")
        XCTAssertFalse(model.deleteConfirm?.removesDirectory ?? true)
        XCTAssertFalse(gate.calls.contains { $0.method == "DELETE" })
        model.cancelDelete()
        XCTAssertNil(model.deleteConfirm)
        model.pressDelete()
        XCTAssertNil(model.deleteConfirm)
        model.requestDelete("a11a0001")
        XCTAssertEqual(
            deleteConfirmLines(
                removesDirectory: model.deleteConfirm?.removesDirectory ?? true,
                workspace: model.deleteConfirm?.workspace ?? ""
            ),
            ["Delete this session?"]
        )
        await model.confirmDelete()
        XCTAssertTrue(gate.calls.contains { $0.method == "DELETE" && $0.path == "/v1/sessions/a11a0001" })
        model.open("a11a0001")
        await model.runPalette(.skill("preflight"))
        XCTAssertEqual(model.draft, "/preflight ")
        XCTAssertNil(model.overlay)
    }

    func testASkillSlashPostsTheTypedText() async throws {
        let gate = try scriptGate()
        let model = try await open(gate)
        model.updateDraft("/preflight")
        await model.send()
        let message = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/messages") })
        XCTAssertEqual(try JSONDecoder().decode(MessageText.self, from: message.body ?? Data()).text, "/preflight")
        XCTAssertFalse(gate.calls.contains { $0.path.contains("SKILL") })
    }

    func testEffortOutsideTheModelPostsAsAMessage() async throws {
        let gate = try scriptGate()
        let model = try await open(gate)
        model.updateDraft("/effort xhigh")
        await model.send()
        let message = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/messages") })
        XCTAssertEqual(try JSONDecoder().decode(MessageText.self, from: message.body ?? Data()).text, "/effort xhigh")
        model.updateDraft("/effort high")
        await model.send()
        let posted = try XCTUnwrap(gate.calls.last { $0.method == "POST" && $0.path.hasSuffix("/model/session") })
        XCTAssertEqual(try JSONDecoder().decode(ModelPayload.self, from: posted.body ?? Data()).effort, "high")
        model.updateDraft("/effort")
        await model.send()
        XCTAssertEqual(model.overlay, .effort)
        XCTAssertEqual(effortChoices(for: "grok-4.7", in: model.models), ["low", "medium", "high"])
    }

    func testTheProfileCommandChangesTheLiveSession() async throws {
        let sessions = try commandFixture("sessions.json")
        let rows = try JSONDecoder().decode([Session].self, from: sessions)
        let names = Data("[\"planning\",\"review\"]".utf8)
        let view = try commandFixture("view.json")
        let stored = LockedProfile()
        let gate = Gate()
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path == "/v1/profiles" {
                return HostResponse(status: 200, body: names)
            }
            if path.hasSuffix("/profile") {
                XCTAssertEqual(request.httpMethod, "POST")
                XCTAssertEqual(path, "/v1/sessions/a11a0001/profile")
                let body = try JSONDecoder().decode(ProfileBody.self, from: request.httpBody ?? Data())
                stored.set(body.profile)
                return HostResponse(status: 204, body: Data())
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: view)
            }
            var listed = rows
            let posted = stored.value
            if !posted.isEmpty {
                listed[1].profile = posted
            }
            return HostResponse(status: 200, body: try JSONEncoder().encode(listed))
        }
        let model = try await open(gate)
        let opened = await model.runPalette(.profile)
        XCTAssertEqual(opened, .finished)
        XCTAssertEqual(model.overlay, .profile)
        XCTAssertEqual(model.profiles, ["planning", "review"])
        XCTAssertEqual(profileChoices(model.profiles), ["planning", "review", "Everything"])
        await model.applyProfile("review")
        XCTAssertEqual(stored.value, "review")
        XCTAssertEqual(model.sessions.first { $0.id == "a11a0001" }?.profile, "review")
        XCTAssertNil(model.overlay)
        await model.openProfileList()
        await model.applyProfile("Everything")
        XCTAssertEqual(stored.value, "")
        XCTAssertNil(model.sessions.first { $0.id == "a11a0001" }?.profile)
        XCTAssertFalse(gate.calls.contains { $0.path.contains("config.toml") })
        XCTAssertFalse(gate.calls.contains { $0.path.hasSuffix("/model/session") })
        let sheet = try String(
            contentsOf: URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent()
                .deletingLastPathComponent()
                .deletingLastPathComponent()
                .appendingPathComponent("App")
                .appendingPathComponent("CommandSheets.swift"),
            encoding: .utf8
        )
        XCTAssertTrue(sheet.contains("profile-everything"))
        XCTAssertTrue(sheet.contains("profile-row-"))
    }

    private func posts(_ gate: Gate) -> [String] {
        gate.calls.filter { $0.method == "POST" }.map(\.path)
    }

    func testGoalControlsPostMessagesDuringAQuestionAndKeepTheDraft() async throws {
        let gate = try scriptGate()
        let model = try await open(gate)
        model.updateDraft("Keep this unsent")
        await model.goalCommand("pause")
        let posted = try XCTUnwrap(gate.calls.last { $0.path.hasSuffix("/messages") })
        XCTAssertEqual(try JSONDecoder().decode(MessageText.self, from: posted.body ?? Data()).text, "/goal pause")
        XCTAssertEqual(model.draft, "Keep this unsent")
        XCTAssertFalse(gate.calls.contains { $0.path.hasSuffix("/answers") })
        model.updateDraft("/goal pause")
        await model.goalCommand("pause")
        XCTAssertEqual(model.draft, "/goal pause")
        let calls = gate.calls.count
        await model.goalCommand("unexpected")
        XCTAssertEqual(gate.calls.count, calls)
    }

    private func open(_ gate: Gate) async throws -> AppModel {
        let directory = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
            .appendingPathComponent("kyoto-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let model = AppModel(
            store: MemoryBaseURL(),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: ViewCache(directory: directory.appendingPathComponent("views")),
            preferences: LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!),
            transport: { gate }
        )
        model.baseURLText = "https:" + String(repeating: "/", count: 2) + "box.tailnet.ts.net:7841"
        await model.connect()
        model.open("a11a0001")
        return model
    }

    private func scriptGate() throws -> Gate {
        let sessions = try commandFixture("sessions.json")
        let view = try commandFixture("view.json")
        let models = try commandFixture("models.json")
        let gate = Gate()
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path == "/v1/models" {
                return HostResponse(status: 200, body: models)
            }
            if path.hasSuffix("/compact") {
                return HostResponse(status: 202, body: Data())
            }
            if path.hasSuffix("/yolo") || path.hasSuffix("/model/session") || path.hasSuffix("/cancel") {
                return HostResponse(status: 204, body: Data())
            }
            if request.httpMethod == "DELETE" {
                return HostResponse(status: 204, body: Data())
            }
            if path.hasSuffix("/messages") {
                return HostResponse(status: 202, body: Data("{\"turnId\":\"t1\"}".utf8))
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: view)
            }
            return HostResponse(status: 200, body: sessions)
        }
        return gate
    }

}

private func commandFixture(_ name: String) throws -> Data {
    let url = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .appendingPathComponent("Fixtures")
        .appendingPathComponent(name)
    return try Data(contentsOf: url)
}

private struct MessageText: Decodable {
    var text: String
}

private struct YoloBody: Decodable {
    var yolo: Bool
}

private struct ProfileBody: Decodable {
    var profile: String
}

private final class LockedProfile: @unchecked Sendable {
    private let lock = NSLock()
    private var stored = ""

    var value: String {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ next: String) {
        lock.lock()
        stored = next
        lock.unlock()
    }


}

extension CommandClientTests {
    @MainActor
    func testLargePastedDraftKeepsOriginalAndSuffix() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let model = AppModel(store: MemoryBaseURL(), drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")), views: ViewCache(directory: directory.appendingPathComponent("views")), preferences: LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!), transport: { Gate() })
        let pasted = String(repeating: "🐕\n", count: 80)
        model.updateComposerDraft(pasted)
        XCTAssertEqual(model.pastedInput, pasted)
        XCTAssertEqual(model.composerDraft, "")
        model.updateComposerDraft("continue")
        XCTAssertEqual(model.composerDraft, "continue")
        XCTAssertEqual(model.draft, pasted + "continue")
        model.removePastedInput()
        XCTAssertNil(model.pastedInput)
        XCTAssertEqual(model.draft, "continue")
    }
}
