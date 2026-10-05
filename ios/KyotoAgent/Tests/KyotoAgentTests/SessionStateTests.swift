import XCTest
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
@testable import KyotoAgent

final class SessionStateTests: XCTestCase {
    func testFixtureRowsMapIntoTheSheets() throws {
        let sheets = SessionSheets(try fixture(View.self, "view.json"))
        let todo = try XCTUnwrap(sheets.todos.first)
        XCTAssertEqual(todo.id, "todo-1")
        XCTAssertEqual(todo.title, "Add eventId")
        XCTAssertEqual(todo.status, .in_progress)
        XCTAssertNil(todo.description)
        XCTAssertTrue(todo.files.isEmpty)
        XCTAssertTrue(todo.links.isEmpty)

        let closeout = try XCTUnwrap(sheets.closeout.first)
        XCTAssertEqual(closeout.id, "cargo-test")
        XCTAssertEqual(closeout.kind, "command")
        XCTAssertEqual(closeout.status, .passed)
        XCTAssertEqual(closeout.attempt, 1)
        XCTAssertEqual(closeout.exit, 0)
        XCTAssertEqual(closeout.tail, "ok")
        XCTAssertEqual(closeoutSummary(closeout), "cargo-test · command · passed · 1 · 0")

        let task = try XCTUnwrap(sheets.tasks.first)
        XCTAssertEqual(task.id, "task01")
        XCTAssertEqual(task.argv, ["cargo", "test", "--offline"])
        XCTAssertEqual(task.state, .running)

        let schedule = try XCTUnwrap(sheets.schedules.first)
        XCTAssertEqual(schedule.id, "sched01")
        XCTAssertEqual(schedule.note, "Check the pull request")
        XCTAssertEqual(schedule.remainingMin, 30)

        XCTAssertEqual(sheets.phase, .thinking)
        XCTAssertEqual(sheets.thinking, "Reading permission_body.")
        let context = try XCTUnwrap(sheets.context)
        XCTAssertEqual(context.used, 12000)
        XCTAssertEqual(context.window, 128000)
        XCTAssertEqual(context.percent, 9)
        XCTAssertEqual(context.buckets.map(\.id), [.system, .tools, .skills, .messages, .free])
        XCTAssertEqual(context.buckets.map(\.tokens), [2000, nil, nil, 10000, nil])
    }

    func testContextKeepsProviderUsageSeparateFromTheCurrentEstimate() throws {
        let data = Data("""
        {"used":12000,"reported_prompt_tokens":9800,"window":128000,"percent":9,"buckets":[]}
        """.utf8)
        let usage = try JSONDecoder().decode(ContextUsage.self, from: data)
        let sheet = try XCTUnwrap(ContextSheet(usage))
        XCTAssertEqual(sheet.used, 12000)
        XCTAssertEqual(sheet.percent, 9)
        XCTAssertEqual(sheet.reportedPromptTokens, 9800)
        let previous = try fixture(View.self, "view.json")
        XCTAssertNil(previous.context?.reported_prompt_tokens)
    }

    func testATodoKeepsItsFilesAndOnlyWebLinksOpen() throws {
        let todo = try JSONDecoder().decode(
            Todo.self,
            from: Data(
                """
                {"id":"t","title":"Write","status":"pending","description":"the body","files":["src/view.rs"],"links":["https://example.com/a","javascript:alert(1)"]}
                """.utf8
            )
        )
        let row = TodoRow(todo)
        XCTAssertEqual(row.description, "the body")
        XCTAssertEqual(row.files, ["src/view.rs"])
        XCTAssertEqual(row.links, ["https://example.com/a", "javascript:alert(1)"])
        XCTAssertEqual(safariURL(row.links[0])?.host, "example.com")
        XCTAssertNil(safariURL(row.links[1]))
        XCTAssertNil(safariURL("notaurl"))
    }

    func testATruncatedFileStaysMarked() throws {
        var file = try fixture(File.self, "file.json")
        XCTAssertFalse(FileSheet(file).truncated)
        file.truncated = true
        let sheet = FileSheet(file)
        XCTAssertEqual(sheet.path, "src/view.rs")
        XCTAssertEqual(sheet.text, "fn permission_body\n")
        XCTAssertTrue(sheet.truncated)
    }

    func testAContextWithoutAPercentDoesNotOpen() throws {
        var view = try fixture(View.self, "view.json")
        view.context = ContextUsage(used: 10, window: 100, percent: nil, buckets: [])
        XCTAssertNil(SessionSheets(view).context)
    }

    func testBannerPhrasesFollowTheTwoLists() throws {
        XCTAssertEqual(bannerPhrase(previous: .working, status: .waiting, waiting: "question"), "needs a question")
        XCTAssertEqual(bannerPhrase(previous: .idle, status: .waiting, waiting: "permission"), "needs a permission")
        XCTAssertEqual(bannerPhrase(previous: nil, status: .waiting, waiting: nil), "needs a permission")
        XCTAssertEqual(bannerPhrase(previous: .working, status: .waiting, waiting: "enhance"), "needs a prompt")
        XCTAssertEqual(bannerPhrase(previous: .idle, status: .waiting, waiting: "enhance"), "needs a prompt")
        XCTAssertEqual(bannerPhrase(previous: nil, status: .waiting, waiting: "enhance"), "needs a prompt")
        XCTAssertEqual(bannerPhrase(previous: .working, status: .idle, waiting: nil), "finished")
        XCTAssertEqual(bannerPhrase(previous: .waiting, status: .idle, waiting: "question"), "finished")
        XCTAssertNil(bannerPhrase(previous: .waiting, status: .waiting, waiting: "permission"))
        XCTAssertNil(bannerPhrase(previous: .idle, status: .working, waiting: nil))
        XCTAssertNil(bannerPhrase(previous: .working, status: .working, waiting: nil))
        XCTAssertNil(bannerPhrase(previous: nil, status: .idle, waiting: nil))
    }

    func testTheOpenSessionDoesNotBannerItself() throws {
        let previous = [
            try session("c0ffee01", .working),
            try session("a11a0001", .working),
        ]
        let next = [
            try session("c0ffee01", .waiting, waiting: "question"),
            try session("a11a0001", .idle),
        ]
        let raised = sessionTransitions(previous: previous, next: next, openId: "a11a0001")
        XCTAssertEqual(raised, [SessionTransition(sessionId: "c0ffee01", phrase: "needs a question")])
    }

    func testTheListPollStopsWhenTheAppIsSuspended() throws {
        XCTAssertFalse(sessionListPolls(connected: true, sceneIsActive: false))
        XCTAssertFalse(openViewPolls(connected: true, sceneIsActive: false, sessionOpen: true))
        XCTAssertTrue(sessionListPolls(connected: true, sceneIsActive: true))
        XCTAssertFalse(openViewPolls(connected: true, sceneIsActive: true, sessionOpen: false))
        let roots = [packageRoot().appendingPathComponent("Sources"), packageRoot().appendingPathComponent("App")]
        for root in roots {
            let enumerator = FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil)
            var saw = false
            while let url = enumerator?.nextObject() as? URL {
                guard url.pathExtension == "swift" else { continue }
                saw = true
                let text = try String(contentsOf: url, encoding: .utf8)
                XCTAssertFalse(text.contains("UNUserNotificationCenter"), url.path)
                XCTAssertFalse(text.contains("UserNotifications"), url.path)
                XCTAssertFalse(text.contains("BGTaskScheduler"), url.path)
            }
            XCTAssertTrue(saw, root.path)
        }
        let panes = try String(contentsOf: packageRoot().appendingPathComponent("App/SessionPanes.swift"), encoding: .utf8)
        XCTAssertTrue(panes.contains("file-truncated"))
        XCTAssertTrue(panes.contains("task-tail"))
        XCTAssertTrue(panes.contains("closeout-tail"))
        XCTAssertTrue(panes.contains("session-banner"))
        XCTAssertTrue(panes.contains("pane-column"))
        XCTAssertTrue(panes.contains("file-sheet"))
        let transcript = try String(contentsOf: packageRoot().appendingPathComponent("App/TranscriptScreen.swift"), encoding: .utf8)
        XCTAssertTrue(transcript.contains("phase-button"))
        XCTAssertTrue(transcript.contains("context-percent"))
        XCTAssertTrue(transcript.contains("composer-queue"))
        XCTAssertTrue(transcript.contains("axis: .vertical"))
        XCTAssertFalse(transcript.contains("dock-todos"))
        XCTAssertTrue(transcript.contains("skill-match-"))
        let root = try String(contentsOf: packageRoot().appendingPathComponent("App/ConnectScreen.swift"), encoding: .utf8)
        XCTAssertTrue(root.contains("NavigationSplitView"))
        XCTAssertTrue(root.contains("columnPlan"))
        XCTAssertTrue(root.contains("PaneColumn"))
        XCTAssertTrue(root.contains("columns-3"))
    }

    func testAPhoneStaysOneColumnAndAPadGainsThePaneColumnWhenAPaneHasRows() throws {
        var view = try fixture(View.self, "view.json")
        XCTAssertTrue(panesHaveRows(SessionSheets(view)))
        XCTAssertEqual(
            columnPlan(width: .phone, panesHaveRows: true),
            ColumnPlan(columns: 1, paneColumn: false)
        )
        XCTAssertEqual(
            columnPlan(width: .pad, panesHaveRows: true),
            ColumnPlan(columns: 3, paneColumn: true)
        )
        view.todos = []
        view.closeout = []
        view.tasks = []
        view.schedules = []
        XCTAssertFalse(panesHaveRows(SessionSheets(view)))
        XCTAssertFalse(panesHaveRows(nil))
        XCTAssertEqual(
            columnPlan(width: .pad, panesHaveRows: false),
            ColumnPlan(columns: 2, paneColumn: false)
        )
        XCTAssertEqual(
            columnPlan(width: .phone, panesHaveRows: false),
            ColumnPlan(columns: 1, paneColumn: false)
        )
    }

    func testTheComposerHasNoDockChips() throws {
        XCTAssertEqual(composerChips(width: .phone, panesHaveRows: true), [])
        XCTAssertEqual(composerChips(width: .phone, panesHaveRows: false), [])
        XCTAssertEqual(composerChips(width: .pad, panesHaveRows: true), [])
        XCTAssertEqual(composerChips(width: .pad, panesHaveRows: false), [])
        let catalog = commandCatalog(skills: []).map(\.id)
        XCTAssertTrue(catalog.contains("yolo"))
        let palette = try String(contentsOf: packageRoot().appendingPathComponent("App/CommandSheets.swift"), encoding: .utf8)
        XCTAssertTrue(palette.contains("palette-todos"))
        XCTAssertTrue(palette.contains("palette-closeout"))
        XCTAssertTrue(palette.contains("palette-tasks"))
        XCTAssertTrue(palette.contains("palette-row-"))
    }

    func testTheChatTitleSharesTheBackButtonLine() throws {
        let line = transcriptBar(name: "fix the dock", phase: "thinking", percent: 9, yolo: "yolo", badge: "working")
        XCTAssertEqual(line.title, "fix the dock")
        XCTAssertTrue(line.onBackLine)
        XCTAssertNil(line.largeTitle)
        XCTAssertEqual(line.under, ["thinking", "9%", "yolo", "working"])
        XCTAssertNotEqual(line.title, "yolo")
        let namedYolo = transcriptBar(name: "yolo", phase: nil, percent: nil, yolo: "yolo", badge: "idle")
        XCTAssertEqual(namedYolo.title, "yolo")
        XCTAssertEqual(namedYolo.under, ["yolo", "idle"])
        XCTAssertTrue(namedYolo.onBackLine)
        let blank = transcriptBar(name: "  ", phase: nil, percent: nil, yolo: "yolo", badge: nil)
        XCTAssertEqual(blank.title, "")
        XCTAssertFalse(blank.onBackLine)
        XCTAssertNil(blank.largeTitle)
        XCTAssertEqual(blank.under, ["yolo"])
        let transcript = try String(contentsOf: packageRoot().appendingPathComponent("App/TranscriptScreen.swift"), encoding: .utf8)
        XCTAssertTrue(transcript.contains("transcriptBar("))
        XCTAssertTrue(transcript.contains(".navigationTitle(bar.title)"))
        XCTAssertTrue(transcript.contains(".navigationBarTitleDisplayMode(.inline)"))
        XCTAssertTrue(transcript.contains("bar.under"))
        XCTAssertFalse(transcript.contains("largeTitle"))
        XCTAssertFalse(transcript.contains(".navigationTitle(\"\")"))
        XCTAssertTrue(transcript.contains("activity-yolo"))
        XCTAssertTrue(transcript.contains("headerYoloWord"))
    }

    func testTheOpenTranscriptShowsYoloApartFromTheStatusBadge() throws {
        let on = try session("a11a0001", .working)
        XCTAssertTrue(on.yolo)
        XCTAssertEqual(headerYoloWord(on), "yolo")
        XCTAssertNotEqual(sessionLine(on, depth: 0).badge, "yolo")
        XCTAssertTrue(sessionLine(on, depth: 0).marks.contains("yolo"))
        var off = on
        off.yolo = false
        XCTAssertNil(headerYoloWord(off))
        XCTAssertFalse(sessionLine(off, depth: 0).marks.contains("yolo"))
    }

    func testTheHeaderShowsTheProfileOnlyWhenTheSessionHasOne() throws {
        var named = try session("a11a0001", .idle)
        named.profile = "review"
        XCTAssertEqual(headerProfileWord(named), "review")
        XCTAssertNotEqual(sessionLine(named, depth: 0).badge, "review")
        var blank = named
        blank.profile = nil
        XCTAssertNil(headerProfileWord(blank))
        blank.profile = "  "
        XCTAssertNil(headerProfileWord(blank))
        let line = transcriptBar(
            name: "fix the dock",
            phase: nil,
            percent: nil,
            yolo: nil,
            profile: headerProfileWord(named),
            badge: "idle"
        )
        XCTAssertEqual(line.under, ["review", "idle"])
        let quiet = transcriptBar(name: "fix the dock", phase: nil, percent: nil, yolo: nil, badge: "idle")
        XCTAssertEqual(quiet.under, ["idle"])
        let transcript = try String(contentsOf: packageRoot().appendingPathComponent("App/TranscriptScreen.swift"), encoding: .utf8)
        XCTAssertTrue(transcript.contains("header-profile"))
        XCTAssertTrue(transcript.contains("headerProfileWord"))
        let screen = try String(contentsOf: packageRoot().appendingPathComponent("App/NewSessionScreen.swift"), encoding: .utf8)
        XCTAssertTrue(screen.contains("new-profile-everything"))
        XCTAssertTrue(screen.contains("profileChoices"))
    }

    func testAnAskSitsOnTheTrailingEdgeAndAResultStaysLeading() {
        XCTAssertEqual(cardEdge(.ask), .trailing)
        XCTAssertEqual(cardEdge(.answer), .trailing)
        XCTAssertEqual(cardEdge(.question), .leading)
        XCTAssertEqual(cardEdge(.permission), .leading)
        XCTAssertEqual(cardEdge(.result), .leading)
        XCTAssertEqual(cardEdge(.proof), .leading)
        XCTAssertEqual(cardEdge(.enhance), .leading)
    }

    func testEscapeClosesThePopupAndControlXCancels() throws {
        XCTAssertEqual(
            hardwareKeyAction(key: .escape, popupOpen: true, enhanceOpen: true),
            .dismissPopup
        )
        XCTAssertNotEqual(
            hardwareKeyAction(key: .escape, popupOpen: true, enhanceOpen: false),
            .cancelTurn
        )
        XCTAssertNil(hardwareKeyAction(key: .escape, popupOpen: false, enhanceOpen: false))
        XCTAssertEqual(
            hardwareKeyAction(key: .escape, popupOpen: false, enhanceOpen: true),
            .discardEnhance
        )
        XCTAssertEqual(
            hardwareKeyAction(key: .controlX, popupOpen: true, enhanceOpen: true),
            .cancelTurn
        )
        XCTAssertEqual(enhanceAnswer(.use).choice, "use")
        XCTAssertNil(enhanceAnswer(.use).text)
        XCTAssertEqual(enhanceAnswer(.edit("sharper")).choice, "revise")
        XCTAssertEqual(enhanceAnswer(.edit("sharper")).text, "sharper")
        XCTAssertEqual(enhanceAnswer(.discard).choice, "discard")
        let app = packageRoot().appendingPathComponent("App")
        let transcript = try String(contentsOf: app.appendingPathComponent("TranscriptScreen.swift"), encoding: .utf8)
        XCTAssertTrue(transcript.contains("keyboardShortcut(\"x\", modifiers: .control)"))
        XCTAssertTrue(transcript.contains("hardwareKeyAction"))
        XCTAssertTrue(transcript.contains("cancelTurn()"))
        XCTAssertTrue(transcript.contains("enhance-edit"))
        XCTAssertTrue(transcript.contains("enhance-use"))
        XCTAssertTrue(transcript.contains("enhance-discard"))
        XCTAssertTrue(transcript.contains("dismissPermission"))
        let panes = try String(contentsOf: app.appendingPathComponent("SessionPanes.swift"), encoding: .utf8)
        XCTAssertTrue(panes.contains("pane-column"))
        XCTAssertTrue(panes.contains("file-sheet"))
        let sessions = try String(contentsOf: app.appendingPathComponent("SessionScreen.swift"), encoding: .utf8)
        XCTAssertTrue(sessions.contains("contextMenu"))
        XCTAssertTrue(sessions.contains("requestDelete"))
        XCTAssertFalse(sessions.contains(".sheet("))
    }

    func testACardListTapRequestsKeyboardDismissalAndALinkOrFieldDoesNot() throws {
        XCTAssertTrue(transcriptRequestsKeyboardDismissal(.cardList))
        XCTAssertTrue(transcriptRequestsKeyboardDismissal(.scroll))
        XCTAssertFalse(transcriptRequestsKeyboardDismissal(.control))
        XCTAssertFalse(transcriptRequestsKeyboardDismissal(.link))
        XCTAssertFalse(transcriptRequestsKeyboardDismissal(.field))
        XCTAssertEqual(transcriptPointerForCardTap(holdsLink: false), .cardList)
        XCTAssertEqual(transcriptPointerForCardTap(holdsLink: true), .link)
        XCTAssertFalse(transcriptRequestsKeyboardDismissal(transcriptPointerForCardTap(holdsLink: true)))
        XCTAssertTrue(transcriptRequestsKeyboardDismissal(transcriptPointerForCardTap(holdsLink: false)))
        XCTAssertTrue(cardHoldsLink(.result(ResultCard(text: "see https://example.com", note: nil))))
        XCTAssertFalse(cardHoldsLink(.ask(TextCard(text: "open the file"))))
        let transcript = try String(contentsOf: packageRoot().appendingPathComponent("App/TranscriptScreen.swift"), encoding: .utf8)
        XCTAssertFalse(transcript.contains("composerChips"))
        XCTAssertFalse(transcript.contains("Color.white.opacity"))
        XCTAssertTrue(transcript.contains("headerYoloWord"))
        XCTAssertTrue(transcript.contains("cardEdge"))
        XCTAssertTrue(transcript.contains("transcriptRequestsKeyboardDismissal"))
        XCTAssertTrue(transcript.contains("scrollDismissesKeyboard"))
        XCTAssertTrue(transcript.contains("activity-yolo"))
        XCTAssertEqual(transcriptDoubleTap(.cardList), .openPalette)
        XCTAssertEqual(transcriptDoubleTap(.scroll), .openPalette)
        XCTAssertEqual(transcriptDoubleTap(.link), .openPalette)
        XCTAssertEqual(transcriptDoubleTap(.field), .ignore)
        XCTAssertEqual(transcriptDoubleTap(.control), .ignore)
        XCTAssertTrue(transcript.contains("transcriptDoubleTap"))
        XCTAssertTrue(transcript.contains("openPalette()"))
        XCTAssertTrue(transcript.contains("TapGesture(count: 2)"))
        XCTAssertTrue(transcript.contains("axis: .vertical"))
        XCTAssertTrue(transcript.contains("composer-send"))
        XCTAssertTrue(transcript.contains("skill-match-"))
        XCTAssertFalse(transcript.contains("\"More\""))
    }

    func testASessionTapShowsDetailForTheCurrentIdAndADifferentId() throws {
        XCTAssertEqual(compactColumn(after: .session("a11a0001"), selection: "a11a0001"), .detail)
        XCTAssertEqual(compactColumn(after: .session("b0b0b0b0"), selection: "a11a0001"), .detail)
        XCTAssertEqual(compactColumn(after: .session("b0b0b0b0"), selection: nil), .detail)
        XCTAssertEqual(compactColumn(after: .pull, selection: "a11a0001"), .list)
        XCTAssertEqual(compactColumn(after: .session(""), selection: nil), .list)
        XCTAssertEqual(preferredCompactSlot(showTranscript: true, paneColumn: true), .content)
        XCTAssertEqual(preferredCompactSlot(showTranscript: true, paneColumn: false), .detail)
        XCTAssertEqual(preferredCompactSlot(showTranscript: false, paneColumn: true), .sidebar)
        XCTAssertEqual(preferredCompactSlot(showTranscript: false, paneColumn: false), .sidebar)
        let screen = try String(contentsOf: packageRoot().appendingPathComponent("App/SessionScreen.swift"), encoding: .utf8)
        XCTAssertTrue(screen.contains("openSession(node.id)"))
        XCTAssertTrue(screen.contains("model.openPull(node.session)"))
        let root = try String(contentsOf: packageRoot().appendingPathComponent("App/ConnectScreen.swift"), encoding: .utf8)
        XCTAssertTrue(root.contains("compactColumn(after:"))
        XCTAssertTrue(root.contains("model.open(id)"))
    }
}

@MainActor
final class SessionClientTests: XCTestCase {
    func testLateFileRepliesDoNotChangeTheNewChat() async throws {
        for status in [200, 403] {
            let gate = Gate()
            let sessions = try fixtureData("sessions.json")
            gate.handler = { _ in HostResponse(status: 200, body: sessions) }
            let transport = DelayedSessionResponse(gate: gate, suffix: "/file")
            let directory = temporaryDirectory()
            let model = model(directory: directory, gate: transport)
            model.baseURLText = sampleBase()
            await model.connect()
            model.open("a11a0001")
            let request = Swift.Task { await model.openFile("src/view.rs") }
            await transport.waitForRequests(1)
            model.open("another-chat")
            let body = status == 200 ? try fixtureData("file.json") : Data("old chat failure".utf8)
            await transport.finishNext(HostResponse(status: status, body: body))
            await request.value
            XCTAssertNil(model.openedFile)
            XCTAssertNil(model.fileFailure)
        }
    }

    func testLateFileReplyDoesNotReopenAfterReturningToTheChat() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        gate.handler = { _ in HostResponse(status: 200, body: sessions) }
        let transport = DelayedSessionResponse(gate: gate, suffix: "/file")
        let model = model(directory: temporaryDirectory(), gate: transport)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        let request = Swift.Task { await model.openFile("src/view.rs") }
        await transport.waitForRequests(1)
        model.open("another-chat")
        model.open("a11a0001")
        await transport.finishNext(HostResponse(status: 200, body: try fixtureData("file.json")))
        await request.value
        XCTAssertNil(model.openedFile)
    }

    func testLateTaskReplyDoesNotReplaceTheOtherChatsTask() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        gate.handler = { _ in HostResponse(status: 200, body: sessions) }
        let transport = DelayedSessionResponse(gate: gate, suffix: "/tasks/task01")
        let model = model(directory: temporaryDirectory(), gate: transport)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        let first = Swift.Task { await model.openTask("task01") }
        await transport.waitForRequests(1)
        model.open("another-chat")
        let second = Swift.Task { await model.openTask("task01") }
        await transport.waitForRequests(2)
        let task = HostResponse(status: 200, body: try fixtureData("task.json"))
        await transport.finishNext(task)
        await first.value
        XCTAssertNil(model.openedTask)
        await transport.finishNext(task)
        await second.value
        XCTAssertNotNil(model.openedTask)
    }

    func testAnOlderConnectionCannotReplaceTheNewerServer() async throws {
        let transport = DelayedSessionResponse(gate: Gate(), suffix: "/sessions")
        let model = model(directory: temporaryDirectory(), gate: transport)
        model.baseURLText = sampleBase()
        let first = Swift.Task { await model.connect() }
        await transport.waitForRequests(1)
        model.baseURLText = "https:" + String(repeating: "/", count: 2) + "other.tailnet.ts.net:7841"
        let second = Swift.Task { await model.connect() }
        await transport.waitForRequests(2)
        await transport.finishNext(HostResponse(status: 200, body: try fixtureData("sessions.json")))
        await first.value
        XCTAssertFalse(model.connected)
        XCTAssertTrue(model.connecting)
        await transport.finishNext(HostResponse(status: 200, body: Data("[]".utf8)))
        await second.value
        XCTAssertTrue(model.connected)
        XCTAssertFalse(model.connecting)
        XCTAssertTrue(model.sessions.isEmpty)
    }

    func testProjectsReloadOnTheNewServerWhileAnOldQueryIsPending() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        gate.handler = { _ in HostResponse(status: 200, body: sessions) }
        let transport = DelayedSessionResponse(gate: gate, suffix: "/projects")
        let model = model(directory: temporaryDirectory(), gate: transport)
        model.baseURLText = sampleBase()
        await model.connect()
        let old = Swift.Task { await model.loadProjects() }
        await transport.waitForRequests(1)
        model.baseURLText = "https:" + String(repeating: "/", count: 2) + "other.tailnet.ts.net:7841"
        let connecting = Swift.Task { await model.connect() }
        await transport.waitForRequests(2)
        await transport.finishNext(HostResponse(status: 403, body: Data("old failure".utf8)))
        await old.value
        XCTAssertFalse(model.projectsReady)
        await transport.finishNext(HostResponse(status: 403, body: Data("new failure".utf8)))
        await connecting.value
        XCTAssertTrue(model.projectsReady)
        XCTAssertEqual(model.projectFailure, "new failure")
    }

    func testViewPollingDoesNotInvalidateAnOpenTaskRequest() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        gate.handler = { request in
            HostResponse(status: 200, body: request.url?.path.hasSuffix("/view") == true ? view : sessions)
        }
        let transport = DelayedSessionResponse(gate: gate, suffix: "/tasks/task01")
        let model = model(directory: temporaryDirectory(), gate: transport)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        let opening = Swift.Task { await model.openTask("task01") }
        await transport.waitForRequests(1)
        let polling = Swift.Task { await model.refreshOpenView() }
        await transport.waitForRequests(2)
        let task = HostResponse(status: 200, body: try fixtureData("task.json"))
        await transport.finishNext(task)
        await opening.value
        XCTAssertNotNil(model.openedTask)
        await transport.finishNext(task)
        await polling.value
        XCTAssertNotNil(model.openedTask)
    }

    func testLateModelRepliesDoNotOpenThePickerOnAnotherServer() async throws {
        for status in [200, 403] {
            let gate = Gate()
            let sessions = try fixtureData("sessions.json")
            gate.handler = { _ in HostResponse(status: 200, body: sessions) }
            let transport = DelayedSessionResponse(gate: gate, suffix: "/models")
            let model = model(directory: temporaryDirectory(), gate: transport)
            model.baseURLText = sampleBase()
            await model.connect()
            let request = Swift.Task { await model.openModelList() }
            await transport.waitForRequests(1)
            model.presentNewSession = true
            model.baseURLText = "https:" + String(repeating: "/", count: 2) + "other.tailnet.ts.net:7841"
            await model.connect()
            let body = status == 200 ? try fixtureData("models.json") : Data("old server failure".utf8)
            await transport.finishNext(HostResponse(status: status, body: body))
            await request.value
            XCTAssertFalse(model.presentNewSession)
            XCTAssertTrue(model.models.isEmpty)
            XCTAssertNil(model.notice)
            XCTAssertNil(model.overlay)
        }
    }

    func testATodoFileOpensTheTextAndDoesNotCacheIt() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        var truncated = try fixture(File.self, "file.json")
        truncated.truncated = true
        truncated.text = "cut\n"
        let files = Locked([
            try JSONEncoder().encode(fixture(File.self, "file.json")),
            try JSONEncoder().encode(truncated),
        ])
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/file") {
                XCTAssertEqual(request.httpMethod, "GET")
                XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
                XCTAssertEqual(request.url?.query, "path=src/view.rs")
                return HostResponse(status: 200, body: files.take())
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: view)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let directory = temporaryDirectory()
        let model = model(directory: directory, gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        await model.openFile("src/view.rs")
        XCTAssertEqual(model.openedFile?.text, "fn permission_body\n")
        XCTAssertFalse(model.openedFile?.truncated ?? true)
        await model.openFile("src/view.rs")
        XCTAssertEqual(model.openedFile?.text, "cut\n")
        XCTAssertEqual(model.openedFile?.truncated, true)
        XCTAssertEqual(gate.paths.filter { $0.hasSuffix("/file") }.count, 2)
        let viewsURL = directory.appendingPathComponent("views")
        let views = (try? FileManager.default.contentsOfDirectory(at: viewsURL, includingPropertiesForKeys: nil)) ?? []
        for url in views {
            let stored = try String(contentsOf: url, encoding: .utf8)
            XCTAssertFalse(stored.contains("fn permission_body"))
            XCTAssertFalse(stored.contains("\"truncated\""))
        }
        let spaced = serveRequest(
            baseURL: try XCTUnwrap(URL(string: sampleBase())),
            call: .file(id: "a11a0001", path: "my file.rs")
        )
        XCTAssertEqual(spaced.url?.query, "path=my%20file.rs")
        let taskCall = serveRequest(
            baseURL: try XCTUnwrap(URL(string: sampleBase())),
            call: .task(id: "a11a0001", taskId: "task01")
        )
        XCTAssertEqual(taskCall.url?.path, "/v1/sessions/a11a0001/tasks/task01")
        XCTAssertNil(taskCall.value(forHTTPHeaderField: "Authorization"))
    }

    func testCloseoutTailComesFromTheRow() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        gate.handler = { request in
            if request.url?.path.hasSuffix("/view") == true {
                return HostResponse(status: 200, body: view)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = model(directory: temporaryDirectory(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        await model.refreshOpenView()
        let before = gate.paths.count
        let row = try XCTUnwrap(model.sheets?.closeout.first)
        XCTAssertEqual(row.tail, "ok")
        XCTAssertEqual(closeoutSummary(row), "cargo-test · command · passed · 1 · 0")
        XCTAssertEqual(gate.paths.count, before)
        XCTAssertFalse(gate.paths.contains { $0.contains("/tasks/") || $0.hasSuffix("/file") })
    }

    func testAnOpenTaskRefetchesWhenTheViewRefreshes() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        let tails = Locked(["first\n", "second\n"])
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/tasks/task01") {
                let tail = tails.take()
                let body = Task(id: "task01", argv: ["cargo", "test"], state: .running, exit: nil, tail: tail)
                return HostResponse(status: 200, body: try JSONEncoder().encode(body))
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: view)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = model(directory: temporaryDirectory(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        await model.openTask("task01")
        XCTAssertEqual(model.openedTask?.tail, "first\n")
        XCTAssertEqual(model.openedTask?.argv, ["cargo", "test"])
        XCTAssertEqual(model.openedTask?.state, .running)
        await model.refreshOpenView()
        XCTAssertEqual(model.openedTask?.tail, "second\n")
        XCTAssertEqual(gate.paths.filter { $0.hasSuffix("/tasks/task01") }.count, 2)
        let detail = try fixture(Task.self, "task.json")
        XCTAssertEqual(TaskDetail(detail).tail, "test result: ok. 1 passed\n")
        XCTAssertEqual(TaskDetail(detail).exit, 0)
        XCTAssertEqual(TaskDetail(detail).state, .exited)
    }

    func testAnotherSessionBannersAndTheOpenOneDoesNot() async throws {
        var first = try fixture([Session].self, "sessions.json")
        first[0].status = .idle
        first[0].waiting = nil
        first[1].status = .working
        first[1].waiting = nil
        var second = first
        second[0].status = .waiting
        second[0].waiting = "permission"
        second[1].status = .waiting
        second[1].waiting = "question"
        var third = second
        third[0].status = .working
        third[0].waiting = nil
        var fourth = third
        fourth[0].status = .idle
        let bodies = Locked([
            try JSONEncoder().encode(first),
            try JSONEncoder().encode(second),
            try JSONEncoder().encode(third),
            try JSONEncoder().encode(fourth),
        ])
        let gate = Gate()
        gate.handler = { _ in
            HostResponse(status: 200, body: bodies.take())
        }
        let model = model(directory: temporaryDirectory(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        XCTAssertTrue(model.banners.isEmpty)
        await model.refreshSessions()
        XCTAssertEqual(model.banners.map(\.phrase), ["needs a permission"])
        XCTAssertEqual(model.banners.map(\.sessionId), ["c0ffee01"])
        await model.refreshSessions()
        XCTAssertEqual(model.banners.map(\.phrase), ["needs a permission"])
        await model.refreshSessions()
        XCTAssertEqual(model.banners.map(\.phrase), ["needs a permission", "finished"])
        model.dismissBanner(model.banners[0].id)
        XCTAssertEqual(model.banners.map(\.phrase), ["finished"])
    }

    func testOpenPanesSurviveANewModel() {
        let preferences = LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!)
        let directory = temporaryDirectory()
        let first = AppModel(
            store: MemoryBaseURL(),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: ViewCache(directory: directory.appendingPathComponent("views")),
            preferences: preferences,
            transport: { Gate() }
        )
        first.setPane(.todos, open: true)
        first.setPane(.schedules, open: true)
        first.setPane(.todos, open: false)
        let second = AppModel(
            store: MemoryBaseURL(),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: ViewCache(directory: directory.appendingPathComponent("views")),
            preferences: preferences,
            transport: { Gate() }
        )
        XCTAssertEqual(second.openPanes, [.schedules])
    }

    private func model(directory: URL, gate: any HostTransport) -> AppModel {
        AppModel(
            store: MemoryBaseURL(),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: ViewCache(directory: directory.appendingPathComponent("views")),
            preferences: LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!),
            transport: { gate }
        )
    }

    private func temporaryDirectory() -> URL {
        let url = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
            .appendingPathComponent("kyoto-" + UUID().uuidString, isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func sampleBase() -> String {
        "https:" + String(repeating: "/", count: 2) + "box.tailnet.ts.net:7841"
    }
}

final class Locked<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [Value]

    init(_ values: [Value]) {
        self.values = values
    }

    func take() -> Value {
        lock.lock()
        defer { lock.unlock() }
        return values.removeFirst()
    }
}

private func session(_ id: String, _ status: Status, waiting: String? = nil) throws -> Session {
    var row = try fixture([Session].self, "sessions.json")[0]
    row.id = id
    row.status = status
    row.waiting = waiting
    return row
}

private func fixture<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
    try JSONDecoder().decode(type, from: fixtureData(name))
}

private func fixtureData(_ name: String) throws -> Data {
    let url = iosRoot().appendingPathComponent("Fixtures").appendingPathComponent(name)
    return try Data(contentsOf: url)
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

private actor DelayedSessionResponse: HostTransport {
    let gate: Gate
    let suffix: String
    var requests = 0
    var pending: [CheckedContinuation<HostResponse, Error>] = []
    var waiters: [(Int, CheckedContinuation<Void, Never>)] = []

    init(gate: Gate, suffix: String) {
        self.gate = gate
        self.suffix = suffix
    }

    nonisolated func send(_ request: URLRequest) async throws -> HostResponse {
        try await receive(request)
    }

    private func receive(_ request: URLRequest) async throws -> HostResponse {
        guard request.url?.path.hasSuffix(suffix) == true else {
            return try await gate.send(request)
        }
        return try await withCheckedThrowingContinuation { continuation in
            requests += 1
            pending.append(continuation)
            let ready = waiters.filter { $0.0 <= requests }
            waiters.removeAll { $0.0 <= requests }
            for (_, waiter) in ready { waiter.resume() }
        }
    }

    func waitForRequests(_ count: Int) async {
        guard requests < count else { return }
        await withCheckedContinuation { waiters.append((count, $0)) }
    }

    func finishNext(_ response: HostResponse) {
        pending.removeFirst().resume(returning: response)
    }
}
