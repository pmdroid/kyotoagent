import XCTest
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
@testable import KyotoAgent

@MainActor
final class ClientTests: XCTestCase {
    func testArtifactCommitMetadataAndRetainedCheckAttemptsDecodeWithoutChatCards() throws {
        let body = Data("""
        {"id":"test","kind":"command","hint":"fix","status":"failed","exit":1,"attempt":2,"tail":"failed","runs":[{"id":"test","attempt":1,"exit":0,"timed_out":true,"tail":"timeout","transcript":{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"test-attempt-1.txt","mediaType":"text/plain","size":3,"sha256":"hash","gitSha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}},{"id":"test","attempt":2,"exit":1,"tail":"failed"}]}
        """.utf8)
        let check = try JSONDecoder().decode(CloseoutCheck.self, from: body)
        let row = CloseoutRow(check)
        XCTAssertEqual(row.runs.count, 2)
        XCTAssertEqual(row.runs[0].transcript?.gitSha, String(repeating: "b", count: 40))
        XCTAssertEqual(closeoutAttemptSummary(row.runs[0]), "Attempt 1 · timed out · exit 0")
        XCTAssertEqual(closeoutAttemptSummary(row.runs[1]), "Attempt 2 · failed · exit 1")
        let roundTrip = try JSONDecoder().decode(CloseoutCheck.self, from: JSONEncoder().encode(check))
        XCTAssertEqual(roundTrip, check)
    }

    func testArtifactHistoryIncludesProofFilesAndAllowsOmittedFiles() async throws {
        let gate = Gate()
        gate.handler = { request in
            XCTAssertEqual(request.url?.path, "/v1/sessions/session/artifacts")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer test-token")
            return HostResponse(status: 200, body: Data(#"[{"version":1,"eventId":"e1","turnId":"t1","at":"now","proof":{"text":"Checks passed"}},{"version":2,"eventId":"e2","turnId":"t2","at":"now","proof":{"files":[{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"birthday-card.jpg","mediaType":"image/jpeg","size":421545,"sha256":"hash"}]}}]"#.utf8))
        }
        let client = ServeClient(baseURL: URL(string: "https://example.test")!, transport: gate, token: "test-token")
        let history = try await client.artifacts("session")
        XCTAssertTrue(history[0].proof.files.isEmpty)
        XCTAssertEqual(history[1].proof.files.first?.name, "birthday-card.jpg")
        gate.handler = { _ in HostResponse(status: 503, body: Data("Unavailable".utf8)) }
        do {
            _ = try await client.artifacts("session")
            XCTFail("History errors must remain visible")
        } catch { XCTAssertEqual(error as? HostError, .status(503, "Unavailable")) }
    }

    func testArtifactsUseTheAuthenticatedArchiveAndRejectChangedBytes() async throws {
        let gate = Gate()
        let bytes = Data("abc".utf8)
        let file = ArtifactFile(id: String(repeating: "a", count: 32), name: "report.md", mediaType: "text/markdown", size: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        gate.handler = { request in
            XCTAssertEqual(request.url?.path, "/v1/sessions/session/artifacts/files/" + file.id)
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer test-token")
            return HostResponse(status: 200, body: bytes)
        }
        let client = ServeClient(baseURL: URL(string: "https://example.test")!, transport: gate, token: "test-token")
        let downloaded = try await client.artifact("session", file: file)
        XCTAssertEqual(downloaded, bytes)
        gate.handler = { _ in HostResponse(status: 200, body: Data("abd".utf8)) }
        do {
            _ = try await client.artifact("session", file: file)
            XCTFail("Changed bytes must fail integrity validation")
        } catch { XCTAssertEqual(error as? HostError, .transport("Artifact integrity check failed")) }
        var invalid = file
        invalid.name = "../report.md"
        do {
            _ = try await client.artifact("session", file: invalid)
            XCTFail("Unsafe artifact names must fail")
        } catch { XCTAssertEqual(error as? HostError, .transport("Invalid artifact metadata")) }
    }

    func testArtifactDigestsMatchStandardVectorsAcrossBlockBoundaries() {
        XCTAssertEqual(artifactDigest(Data()), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        XCTAssertEqual(artifactDigest(Data(String(repeating: "a", count: 1000).utf8)), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3")
    }

    func testArtifactCardsDecodeDuringAnUnfinishedTurn() throws {
        let body = Data("{\"id\":\"c2\",\"kind\":\"artifact\",\"at\":\"now\",\"body\":{\"eventId\":\"e2\",\"turnId\":\"t1\",\"file\":{\"id\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"name\":\"report.md\",\"mediaType\":\"text/markdown\",\"size\":3,\"sha256\":\"hash\"}}}".utf8)
        let card = try JSONDecoder().decode(Card.self, from: body)
        guard case .artifact(let artifact) = card.body else { return XCTFail("Expected an artifact") }
        XCTAssertEqual(artifact.file.name, "report.md")
        XCTAssertEqual(artifact.turnId, "t1")
        XCTAssertEqual(cardEdge(card.kind), .leading)
    }

    func testImageOnlyMessagesKeepTheirAttachmentsOnFailureAndClearOnAcceptance() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        let pngURL = packageRoot().deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("assets/kyoto-dog.png")
        let bytes = try Data(contentsOf: pngURL)
        let image = ImageAttachment(name: "dog.png", mimeType: "image/png", data: bytes.base64EncodedString())
        let cardData = try JSONSerialization.data(withJSONObject: [
            "id": "ask-image", "kind": "ask", "at": "2026-10-03T00:00:00Z",
            "body": ["text": "", "images": [["name": image.name, "mimeType": image.mimeType, "data": image.data]]],
        ])
        let card = try JSONDecoder().decode(Card.self, from: cardData)
        guard case .ask(let ask) = card.body else { return XCTFail("Expected an image ask") }
        XCTAssertEqual(ask.images, [image])
        XCTAssertEqual(ask.images?.first?.bytes, bytes)
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/messages") {
                let payload = try JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as! [String: Any]
                XCTAssertEqual(payload["text"] as? String, "")
                let sent = try JSONDecoder().decode([ImageAttachment].self, from: JSONSerialization.data(withJSONObject: payload["images"]!))
                XCTAssertEqual(sent, [image])
                return HostResponse(status: 409, body: Data("{\"error\":\"busy\"}".utf8))
            }
            return HostResponse(status: 200, body: path.hasSuffix("/view") ? view : sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        model.addImage(image)
        XCTAssertEqual(model.images, [image])
        model.open("c0ffee01")
        XCTAssertTrue(model.images.isEmpty)
        model.open("a11a0001")
        await model.send()
        XCTAssertEqual(model.images, [image])
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/messages") {
                return HostResponse(status: 202, body: Data("{\"turnId\":\"t-image\"}".utf8))
            }
            return HostResponse(status: 200, body: path.hasSuffix("/view") ? view : sessions)
        }
        await model.send()
        XCTAssertTrue(model.images.isEmpty)
        model.previewImage(image)
        XCTAssertEqual(model.shownImage, image)
        model.closeImage()
        XCTAssertNil(model.shownImage)
    }

    func testConnectStoresTheURLAndShowsTheList() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        gate.handler = { request in
            XCTAssertEqual(request.httpMethod, "GET")
            XCTAssertEqual(request.url?.path, "/v1/sessions")
            XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
            return HostResponse(status: 200, body: sessions)
        }
        let store = MemoryBaseURL()
        let model = try model(store: store, gate: gate)
        let raw = "https:" + String(repeating: "/", count: 2) + "box.tailnet.ts.net:7841"
        model.baseURLText = "  " + raw + "  "
        await model.connect()
        XCTAssertTrue(model.connected)
        XCTAssertNil(model.failure)
        XCTAssertEqual(store.saves, [raw])
        XCTAssertEqual(model.nodes.map(\.session.id), ["a11a0001", "c0ffee01"])
        XCTAssertEqual(model.nodes.map(\.depth), [0, 1])
        let child = try XCTUnwrap(model.nodes.last)
        let line = sessionLine(child.session, depth: child.depth)
        XCTAssertEqual(line.name, "Permission cards")
        XCTAssertEqual(line.idPrefix, "c0ff")
        XCTAssertEqual(line.badge, "permission")
        XCTAssertEqual(line.status, .waiting)
        XCTAssertTrue(line.marks.contains("child"))
        XCTAssertTrue(line.marks.contains("yolo"))
        XCTAssertTrue(line.marks.contains("compacting"))
        XCTAssertTrue(line.marks.contains("grok-4.7"))
        XCTAssertTrue(line.marks.contains("medium"))
    }

    func testPairingURIStoresCredentialsAndUsesBearerAuthentication() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        gate.handler = { request in
            XCTAssertEqual(request.url?.scheme, "https")
            XCTAssertEqual(request.url?.host, "box.example")
            XCTAssertNil(request.url?.query)
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer signed-token")
            return HostResponse(status: 200, body: sessions)
        }
        let uri = "kyotoagent://box.example:7841?token=signed-token"
        let store = MemoryBaseURL(value: uri)
        let model = try model(store: store, gate: gate)
        await model.connect()
        XCTAssertTrue(model.connected)
        XCTAssertEqual(store.saves, [uri])
        XCTAssertEqual(hostLabel(model.baseURLText), "box.example")
    }

    func testPairingRejectsInvalidConnectionLinks() {
        for invalid in [
            "kyotoagent://box.example:7841",
            "kyotoagent://box.example:7841?token=",
            "kyotoagent://box.example:7841?token=a&token=b",
            "kyotoagent://user:pass@box.example:7841?token=a",
            "kyotoagent://box.example:7841/path?token=a",
            "kyotoagent://box.example:7841?token=a%0D%0Asecret",
            "https://box.example:7841?token=a",
        ] {
            XCTAssertNil(PairingConnection(invalid), invalid)
        }
    }

    func testAFailedConnectShowsTheStatusAndBody() async throws {
        let gate = Gate()
        gate.handler = { _ in
            HostResponse(status: 502, body: Data("bad gateway".utf8))
        }
        let store = MemoryBaseURL()
        let model = try model(store: store, gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        XCTAssertFalse(model.connected)
        XCTAssertEqual(model.failure, "502\nbad gateway")
        XCTAssertTrue(store.saves.isEmpty)
        XCTAssertTrue(model.sessions.isEmpty)
    }

    func testAskOnAnIdleSessionAppearsOnTheNextView() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let turn = try bodyData("turn.json")
        var view = try String(decoding: fixtureData("view.json"), as: UTF8.self)
        view = view.replacingOccurrences(of: "\"status\": \"waiting\"", with: "\"status\": \"idle\"")
        view = view.replacingOccurrences(of: "Add eventId to the permission card.", with: "open http links")
        let viewBody = Data(view.utf8)
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/messages") {
                let payload = try JSONDecoder().decode(TextBody.self, from: request.httpBody ?? Data())
                XCTAssertEqual(payload.text, "open http links")
                XCTAssertEqual(request.httpMethod, "POST")
                XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
                XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
                return HostResponse(status: 202, body: turn)
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: viewBody)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        model.updateDraft("open http links")
        await model.send()
        XCTAssertEqual(model.draft, "")
        XCTAssertNil(model.notice)
        let ask = try XCTUnwrap(model.transcript.view?.cards.first { $0.kind == .ask })
        guard case .ask(let text) = ask.body else {
            XCTFail("expected an ask")
            return
        }
        XCTAssertEqual(text.text, "open http links")
    }

    func testAQueuedAskShowsTheQueueAndKeepsAFullQueueDraft() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let queued = try bodyData("queued.json")
        let conflict = try bodyData("conflict.json")
        var queuedView = try String(decoding: fixtureData("view.json"), as: UTF8.self)
        queuedView = queuedView.replacingOccurrences(
            of: "Check the fixture diff.",
            with: "open http links in Safari"
        )
        queuedView = queuedView.replacingOccurrences(of: "\"revision\": 9", with: "\"revision\": 10")
        let queuedBody = Data(queuedView.utf8)
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/messages") {
                let payload = try JSONDecoder().decode(TextBody.self, from: request.httpBody ?? Data())
                if payload.text == "one more" {
                    return HostResponse(status: 409, body: conflict)
                }
                return HostResponse(status: 202, body: queued)
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: queuedBody)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        model.updateDraft("open http links in Safari")
        await model.send()
        XCTAssertEqual(model.draft, "")
        XCTAssertEqual(model.transcript.view?.queue, ["open http links in Safari"])
        model.updateDraft("one more")
        await model.send()
        XCTAssertEqual(model.draft, "one more")
        XCTAssertEqual(model.notice, "the queue is full")
        XCTAssertEqual(model.transcript.view?.queue, ["open http links in Safari"])
    }

    func testCancelPostsAndExpectsNoContent() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/cancel") {
                XCTAssertEqual(request.httpMethod, "POST")
                XCTAssertNil(request.httpBody)
                XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
                XCTAssertEqual(path, "/v1/sessions/a11a0001/cancel")
                return HostResponse(status: 204, body: Data())
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: view)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        await model.cancelTurn()
        XCTAssertTrue(gate.paths.contains("/v1/sessions/a11a0001/cancel"))
        XCTAssertNil(model.notice)
    }

    func testUnsentComposerTextSurvivesANewModel() async throws {
        let directory = temporaryDirectory()
        let drafts = DraftFiles(directory: directory.appendingPathComponent("drafts"))
        let preferences = LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!)
        let first = AppModel(
            store: MemoryBaseURL(),
            drafts: drafts,
            views: ViewCache(directory: directory.appendingPathComponent("views")),
            preferences: preferences,
            transport: { Gate() }
        )
        first.open("a11a0001")
        first.updateDraft("still typing")
        let second = AppModel(
            store: MemoryBaseURL(),
            drafts: drafts,
            views: ViewCache(directory: directory.appendingPathComponent("views")),
            preferences: preferences,
            transport: { Gate() }
        )
        XCTAssertEqual(preferences.id, "a11a0001")
        second.open("a11a0001")
        XCTAssertEqual(second.draft, "still typing")
    }

    func testProofCardsWithoutItemsDecode() throws {
        let data = Data("""
        {"id":"proof","kind":"proof","at":"now","body":{"text":"File saved."}}
        """.utf8)
        let card = try JSONDecoder().decode(Card.self, from: data)
        guard case .proof(let proof) = card.body else { return XCTFail("Expected proof") }
        XCTAssertEqual(proof.text, "File saved.")
        XCTAssertTrue(proof.items.isEmpty)
    }

    func testFailedRefreshDoesNotAdvertiseCachedWorkAndRecovers() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        var working = try JSONDecoder().decode(View.self, from: fixtureData("view.json"))
        working.status = .working
        let served = Served(try JSONEncoder().encode(working))
        gate.handler = { request in
            HostResponse(status: 200, body: request.url?.path.hasSuffix("/view") == true ? served.data : sessions)
        }
        let model = try model(store: MemoryBaseURL(value: sampleBase()), gate: gate)
        await model.connect()
        model.open("a11a0001")
        await model.refreshOpenView()
        XCTAssertEqual(model.displayedStatus, .working)
        served.data = Data("invalid response".utf8)
        await model.refreshOpenView()
        XCTAssertNil(model.displayedStatus)
        XCTAssertNotNil(model.transcriptFailure)
        XCTAssertEqual(model.transcript.view?.cards, working.cards)
        working.status = .idle
        served.data = try JSONEncoder().encode(working)
        await model.refreshOpenView()
        XCTAssertEqual(model.displayedStatus, .idle)
        XCTAssertNil(model.transcriptFailure)
    }

    func testSameRevisionRefreshesStatusAndLiveMetadata() throws {
        var working = try JSONDecoder().decode(View.self, from: fixtureData("view.json"))
        working.status = .working
        working.phase = .thinking
        working.thinking = "Still working"
        working.queue = ["Queued ask"]
        var window = TranscriptWindow()
        XCTAssertTrue(window.receive(working))
        var finished = working
        finished.status = .idle
        finished.phase = nil
        finished.thinking = nil
        finished.queue = []
        finished.context?.used = 14000
        XCTAssertTrue(window.receive(finished))
        XCTAssertEqual(window.view?.status, .idle)
        XCTAssertNil(window.view?.phase)
        XCTAssertNil(window.view?.thinking)
        XCTAssertEqual(window.view?.queue, [])
        XCTAssertEqual(window.view?.context?.used, 14000)
        XCTAssertEqual(window.view?.cards, working.cards)
        XCTAssertFalse(window.receive(finished))
    }

    func testAnUnchangedRevisionDoesNotReplaceTheTranscript() async throws {
        let directory = temporaryDirectory()
        let cache = ViewCache(directory: directory)
        let original = try JSONDecoder().decode(View.self, from: fixtureData("view.json"))
        try cache.store("a11a0001", original)
        var changed = try String(decoding: fixtureData("view.json"), as: UTF8.self)
        changed = changed.replacingOccurrences(of: "Add eventId to the permission card.", with: "replaced")
        let sameRevision = try JSONDecoder().decode(View.self, from: Data(changed.utf8))
        var window = TranscriptWindow()
        _ = window.receive(try XCTUnwrap(cache.load("a11a0001")))
        XCTAssertFalse(window.receive(sameRevision))
        let ask = try XCTUnwrap(window.view?.cards.first { $0.kind == .ask })
        guard case .ask(let text) = ask.body else {
            XCTFail("expected an ask")
            return
        }
        XCTAssertEqual(text.text, "Add eventId to the permission card.")
        let newer = changed.replacingOccurrences(of: "\"revision\": 9", with: "\"revision\": 10")
        let next = try JSONDecoder().decode(View.self, from: Data(newer.utf8))
        XCTAssertTrue(window.receive(next))
        let updated = try XCTUnwrap(window.view?.cards.first { $0.kind == .ask })
        guard case .ask(let updatedText) = updated.body else {
            XCTFail("expected an ask")
            return
        }
        XCTAssertEqual(updatedText.text, "replaced")
    }

    func testACachedViewPaintsBeforeTheFetchAndTheFetchWins() async throws {
        let directory = temporaryDirectory()
        let cache = ViewCache(directory: directory.appendingPathComponent("views"))
        let original = try JSONDecoder().decode(View.self, from: fixtureData("view.json"))
        try cache.store("a11a0001", original)
        let gate = Gate()
        var newer = try String(decoding: fixtureData("view.json"), as: UTF8.self)
        newer = newer.replacingOccurrences(of: "\"revision\": 9", with: "\"revision\": 10")
        newer = newer.replacingOccurrences(of: "Add eventId to the permission card.", with: "from the host")
        let sessions = try fixtureData("sessions.json")
        let newerBody = Data(newer.utf8)
        gate.handler = { request in
            if request.url?.path.hasSuffix("/view") == true {
                return HostResponse(status: 200, body: newerBody)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = AppModel(
            store: MemoryBaseURL(value: sampleBase()),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: cache,
            preferences: LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!),
            transport: { gate }
        )
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        let painted = try XCTUnwrap(model.transcript.view?.cards.first { $0.kind == .ask })
        guard case .ask(let cached) = painted.body else {
            XCTFail("expected an ask")
            return
        }
        XCTAssertEqual(cached.text, "Add eventId to the permission card.")
        await model.refreshOpenView()
        let fetched = try XCTUnwrap(model.transcript.view?.cards.first { $0.kind == .ask })
        guard case .ask(let live) = fetched.body else {
            XCTFail("expected an ask")
            return
        }
        XCTAssertEqual(live.text, "from the host")
        XCTAssertEqual(cache.load("a11a0001")?.revision, 10)
    }

    func testUnauthenticatedRequestsHaveNoTokenAndNoTerminalRoutes() throws {
        let request = serveRequest(baseURL: try XCTUnwrap(URL(string: sampleBase())), call: .sessions)
        XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
        XCTAssertTrue(request.allHTTPHeaderFields?.isEmpty ?? true)
        let configuration = URLSessionTransport.systemConfiguration()
        XCTAssertNil(configuration.httpAdditionalHeaders)
        XCTAssertNil(configuration.urlCache)
        let sources = packageRoot().appendingPathComponent("Sources")
        let enumerator = FileManager.default.enumerator(at: sources, includingPropertiesForKeys: nil)
        var saw = false
        while let url = enumerator?.nextObject() as? URL {
            guard url.pathExtension == "swift" else { continue }
            saw = true
            let text = try String(contentsOf: url, encoding: .utf8)
            XCTAssertFalse(text.replacingOccurrences(of: modelConfirmSentence, with: "").contains("config.toml"), url.path)
            XCTAssertFalse(text.contains("/v1/login"), url.path)
            XCTAssertFalse(text.contains("/doctor"), url.path)
            XCTAssertFalse(text.contains("UNUserNotificationCenter"), url.path)
            XCTAssertFalse(text.contains("UserNotifications"), url.path)
        }
        XCTAssertTrue(saw)
        XCTAssertEqual(RefreshCadence.sessions, .seconds(2))
        XCTAssertEqual(RefreshCadence.view, .seconds(1))
    }

    func testAProjectAndGitWorktreePostsThePathAndWorktree() async throws {
        let posted = try await create(projects: fixtureData("projects.json"), worktree: true, draft: nil, projectID: "kyotoagent")
        XCTAssertEqual(posted.workspace, "/home/pascal/work/kyotoagent")
        XCTAssertEqual(posted.worktree, true)
        XCTAssertFalse(posted.keys.contains("yolo"))
        XCTAssertFalse(posted.keys.contains("profile"))
    }

    func testThisFolderPostsWorktreeFalse() async throws {
        let posted = try await create(projects: fixtureData("projects.json"), worktree: false, draft: nil, projectID: "notes")
        XCTAssertEqual(posted.workspace, "/home/pascal/notes")
        XCTAssertEqual(posted.worktree, false)
        XCTAssertFalse(posted.keys.contains("yolo"))
        XCTAssertFalse(posted.keys.contains("profile"))
    }

    func testAnEmptyProjectListCreatesFromATypedPath() async throws {
        let posted = try await create(projects: Data("[]".utf8), worktree: true, draft: "/srv/host/app", projectID: nil)
        XCTAssertEqual(posted.workspace, "/srv/host/app")
        XCTAssertEqual(posted.worktree, true)
        XCTAssertFalse(posted.keys.contains("yolo"))
        XCTAssertFalse(posted.keys.contains("profile"))
    }

    func testCreateWithReviewSendsReview() async throws {
        let posted = try await create(
            projects: fixtureData("projects.json"),
            worktree: true,
            draft: nil,
            projectID: "kyotoagent",
            profile: "review",
            profiles: Data("[\"planning\",\"review\"]".utf8)
        )
        XCTAssertEqual(posted.workspace, "/home/pascal/work/kyotoagent")
        XCTAssertEqual(posted.profile, "review")
        XCTAssertTrue(posted.keys.contains("profile"))
    }

    func testCloseSendsDeleteAndDropsTheRow() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let state = Flag()
        gate.handler = { request in
            if request.httpMethod == "DELETE" {
                state.on = true
                XCTAssertEqual(request.url?.path, "/v1/sessions/a11a0001")
                XCTAssertNil(request.url?.query)
                XCTAssertNil(request.httpBody)
                return HostResponse(status: 204, body: Data())
            }
            if state.on {
                let rows = try JSONDecoder().decode([Session].self, from: sessions).filter { $0.id != "a11a0001" }
                return HostResponse(status: 200, body: try JSONEncoder().encode(rows))
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        await model.closeSession("a11a0001")
        XCTAssertEqual(
            gate.calls.filter { $0.method == "DELETE" }.map(\.path),
            ["/v1/sessions/a11a0001"]
        )
        XCTAssertNil(model.notice)
        XCTAssertNil(model.selection)
        XCTAssertFalse(model.sessions.contains { $0.id == "a11a0001" })
    }

    func testDeleteAsksBeforeItRemovesTheSession() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        gate.handler = { request in
            if request.httpMethod == "DELETE" {
                XCTAssertEqual(request.url?.query, "delete_workspace=true")
                return HostResponse(status: 204, body: Data())
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("c0ffee01")
        model.requestDelete("c0ffee01")
        XCTAssertEqual(
            deleteConfirmLines(
                removesDirectory: model.deleteConfirm?.removesDirectory ?? false,
                workspace: model.deleteConfirm?.workspace ?? ""
            ),
            [
                "Delete this session?",
                "/home/pascal/.kyotoagent/worktrees/kyotoagent-c0ffee01",
                "That directory will be removed.",
            ]
        )
        XCTAssertFalse(gate.calls.contains { $0.method == "DELETE" })
        model.cancelDelete()
        model.openPalette()
        model.pressDelete()
        XCTAssertNil(model.deleteConfirm)
        model.dismissOverlay()
        model.pressDelete()
        XCTAssertEqual(model.deleteConfirm?.id, "c0ffee01")
        XCTAssertFalse(gate.calls.contains { $0.method == "DELETE" })
        await model.confirmDelete(deleteWorkspace: true)
        XCTAssertEqual(
            gate.calls.filter { $0.method == "DELETE" }.map(\.path),
            ["/v1/sessions/c0ffee01"]
        )
    }

    func testDeletingAWorktreeSessionKeepsItsDirectoryByDefault() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        gate.handler = { request in
            if request.httpMethod == "DELETE" {
                XCTAssertEqual(request.url?.path, "/v1/sessions/c0ffee01")
                XCTAssertNil(request.url?.query)
                return HostResponse(status: 204, body: Data())
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.requestDelete("c0ffee01")
        XCTAssertTrue(model.deleteConfirm?.removesDirectory == true)
        await model.confirmDelete()
        XCTAssertEqual(gate.calls.filter { $0.method == "DELETE" }.count, 1)
    }

    func testAWorktreeConflictShowsTheServerMessageAndKeepsTheSession() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        gate.handler = { request in
            if request.httpMethod == "DELETE" {
                XCTAssertEqual(request.url?.path, "/v1/sessions/c0ffee01")
                XCTAssertEqual(request.url?.query, "delete_workspace=true")
                return HostResponse(status: 409, body: Data("{\"error\":\"isolation is not worktree\"}".utf8))
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        let before = model.sessions.map(\.id)
        model.requestDelete("c0ffee01")
        await model.confirmDelete(deleteWorkspace: true)
        XCTAssertEqual(model.notice, "isolation is not worktree")
        XCTAssertEqual(model.sessions.map(\.id), before)
    }

    func testThePullSheetHandsTheURLToSafariAndDoesNotFetchIt() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        gate.handler = { _ in
            HostResponse(status: 200, body: sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        let session = try XCTUnwrap(model.sessions.first { $0.pullUrl != nil })
        let before = gate.calls.count
        model.openPull(session)
        let destination = try XCTUnwrap(safariURL(try XCTUnwrap(model.shownPull)))
        XCTAssertEqual(destination.absoluteString, session.pullUrl)
        XCTAssertEqual(gate.calls.count, before)
        XCTAssertFalse(gate.calls.contains { $0.path.contains("github") || $0.path.contains("pull") })
        let quiet = try XCTUnwrap(model.sessions.first { $0.pullUrl == nil })
        model.openPull(quiet)
        XCTAssertNil(model.shownPull)
    }

    func testAnswerJSONUsesTheFixtureEventIdAndChoice() throws {
        let view = try JSONDecoder().decode(View.self, from: fixtureData("view.json"))
        let permission = try XCTUnwrap(AnswerSheet.front(cards: view.cards, dismissed: nil))
        XCTAssertEqual(permission.eventId, "e-perm")
        let permissionCard = try XCTUnwrap(view.cards.first { $0.kind == .permission })
        for choice in [Decision.allow_once, .allow_session, .deny] {
            let payload = AnswerPayload(sheet: permission, choice: choice.rawValue)
            let encoded = try JSONEncoder().encode(payload)
            let decoded = try JSONDecoder().decode(AnswerPayload.self, from: encoded)
            XCTAssertEqual(decoded.id, "e-perm")
            XCTAssertEqual(decoded.choice, choice.rawValue)
            XCTAssertNotEqual(decoded.id, permissionCard.id)
            XCTAssertNotEqual(decoded.id, "c1")
        }
        let questionCard = try XCTUnwrap(view.cards.first { card in
            guard case .question(let body) = card.body else { return false }
            return body.answer == nil
        })
        guard case .question(let question) = questionCard.body else {
            XCTFail("expected the open question")
            return
        }
        let label = try XCTUnwrap(question.choices.first)
        let labeled = AnswerPayload(id: question.eventId, choice: label)
        XCTAssertEqual(labeled.id, "e-open")
        XCTAssertEqual(labeled.choice, "Kyoto Agent")
        XCTAssertNotEqual(labeled.id, questionCard.id)
        let free = AnswerPayload(id: question.eventId, choice: "a free reply")
        let freeJSON = try JSONDecoder().decode(AnswerPayload.self, from: JSONEncoder().encode(free))
        XCTAssertEqual(freeJSON.id, "e-open")
        XCTAssertEqual(freeJSON.choice, "a free reply")
        let settled = view.cards.filter { card in
            switch card.body {
            case .permission(let body):
                return body.decision != nil
            case .question(let body):
                return body.answer != nil
            default:
                return false
            }
        }
        XCTAssertNil(AnswerSheet.front(cards: settled, dismissed: nil))
    }

    func testAnAnswerPostsThePermissionEventId() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        let posted = Posted()
        gate.handler = { request in
            let path = request.url?.path ?? ""
            posted.paths.append(path)
            if path.hasSuffix("/answers") {
                let payload = try JSONDecoder().decode(AnswerPayload.self, from: request.httpBody ?? Data())
                posted.payloads.append(payload)
                XCTAssertEqual(request.httpMethod, "POST")
                XCTAssertEqual(path, "/v1/sessions/a11a0001/answers")
                XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
                XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
                return HostResponse(status: 204, body: Data())
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: view)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try await openSession(gate)
        XCTAssertEqual(model.answerSheet?.eventId, "e-perm")
        await model.answer(Decision.allow_once.rawValue)
        XCTAssertEqual(posted.payloads, [AnswerPayload(id: "e-perm", choice: "allow_once")])
        XCTAssertNil(model.notice)
        XCTAssertFalse(posted.paths.contains { $0.contains("/events") })
    }

    func testAQuestionPostsTheChoiceLabelOrFreeText() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        var view = try String(decoding: fixtureData("view.json"), as: UTF8.self)
        view = view.replacingOccurrences(of: "\"decision\": null", with: "\"decision\": \"allow_once\"")
        let viewBody = Data(view.utf8)
        let posted = Posted()
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/answers") {
                let payload = try JSONDecoder().decode(AnswerPayload.self, from: request.httpBody ?? Data())
                posted.payloads.append(payload)
                return HostResponse(status: 204, body: Data())
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: viewBody)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try await openSession(gate)
        XCTAssertEqual(model.answerSheet?.eventId, "e-open")
        await model.answer("Kyoto Agent")
        await model.answer("a free reply")
        XCTAssertEqual(
            posted.payloads,
            [
                AnswerPayload(id: "e-open", choice: "Kyoto Agent"),
                AnswerPayload(id: "e-open", choice: "a free reply"),
            ]
        )
        XCTAssertFalse(posted.payloads.contains { $0.id == "c2" || $0.id == "c1" })
    }

    func testASettledAnswerShowsTheServerMessage() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/answers") {
                return HostResponse(status: 409, body: Data(#"{"error":"already settled"}"#.utf8))
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: view)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try await openSession(gate)
        await model.answer(Decision.deny.rawValue)
        XCTAssertEqual(model.notice, "already settled")
        XCTAssertEqual(model.answerSheet?.eventId, "e-perm")
    }

    func testADismissedQuestionStaysHiddenUntilTheEventIdChanges() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let served = Served(try questionView())
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: served.data)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let directory = temporaryDirectory()
        let preferences = LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!)
        let cache = ViewCache(directory: directory)
        let model = AppModel(
            store: MemoryBaseURL(),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: cache,
            preferences: preferences,
            transport: { gate }
        )
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        await model.refreshOpenView()
        XCTAssertEqual(model.answerSheet?.eventId, "e-open")
        model.dismissQuestion()
        XCTAssertNil(model.answerSheet)
        XCTAssertEqual(preferences.dismissedQuestion(session: "a11a0001"), "e-open")
        await model.refreshOpenView()
        XCTAssertNil(model.answerSheet)
        let restored = AppModel(
            store: MemoryBaseURL(),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: cache,
            preferences: preferences,
            transport: { gate }
        )
        restored.open("a11a0001")
        XCTAssertNil(restored.answerSheet)
        var next = String(decoding: served.data, as: UTF8.self)
        next = next.replacingOccurrences(of: "e-open", with: "e-next")
        next = next.replacingOccurrences(of: "\"revision\": 9", with: "\"revision\": 10")
        served.data = Data(next.utf8)
        await model.refreshOpenView()
        XCTAssertEqual(model.answerSheet?.eventId, "e-next")
        model.dismissQuestion()
        XCTAssertNil(model.answerSheet)
    }

    func testTheFrontCardIsTheOpenPermission() throws {
        let ordered = try permissionBehindQuestion()
        let front = try XCTUnwrap(AnswerSheet.front(cards: ordered.cards, dismissed: nil))
        guard case .permission(let permission) = front else {
            XCTFail("expected the permission")
            return
        }
        XCTAssertEqual(permission.eventId, "e-perm")
        XCTAssertEqual(permission.action, "Replace src/view.rs")
        XCTAssertEqual(permission.path, "/home/pascal/work/kyotoagent/src/view.rs")
        XCTAssertEqual(permission.diff, ["+        \"eventId\": event.id,"])
        XCTAssertEqual(permission.argv, ["cargo", "test", "--offline"])
        XCTAssertTrue(front.isPermission)
    }

    func testADismissedQuestionDoesNotHideAnOlderPermission() throws {
        let ordered = try permissionBehindQuestion()
        let front = try XCTUnwrap(AnswerSheet.front(cards: ordered.cards, dismissed: "e-open"))
        XCTAssertEqual(front.eventId, "e-perm")
        XCTAssertTrue(front.isPermission)
    }

    func testADismissedQuestionDoesNotHideAnOlderQuestion() throws {
        var ordered = try permissionBehindQuestion()
        ordered.cards = ordered.cards.map { card in
            guard case .permission(var permission) = card.body else {
                return card
            }
            permission.decision = .deny
            var settled = card
            settled.body = .permission(permission)
            return settled
        }
        var older = try XCTUnwrap(ordered.cards.first { card in
            guard case .question(let question) = card.body else {
                return false
            }
            return question.answer != nil
        })
        if case .question(var question) = older.body {
            question.answer = nil
            question.eventId = "e-older"
            older.body = .question(question)
        }
        ordered.cards.removeAll { $0.id == older.id }
        ordered.cards.insert(older, at: 0)
        let front = try XCTUnwrap(AnswerSheet.front(cards: ordered.cards, dismissed: "e-open"))
        XCTAssertEqual(front.eventId, "e-older")
        XCTAssertFalse(front.isPermission)
    }

    func testANilSheetBindingLeavesAnOpenPermission() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let ordered = try permissionBehindQuestion()
        let body = try JSONEncoder().encode(ordered)
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: body)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try await openSession(gate)
        XCTAssertEqual(model.answerSheet?.eventId, "e-perm")
        XCTAssertEqual(model.answerSheet?.isPermission, true)
        model.dismissQuestion()
        XCTAssertEqual(model.answerSheet?.eventId, "e-perm")
        XCTAssertNil(model.dismissedQuestion)
    }

    func testADismissedQuestionStillShowsTheOlderPermission() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let ordered = try permissionBehindQuestion()
        let body = try JSONEncoder().encode(ordered)
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: body)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let directory = temporaryDirectory()
        let preferences = LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!)
        preferences.dismissQuestion("e-open", session: "a11a0001")
        let model = AppModel(
            store: MemoryBaseURL(),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: ViewCache(directory: directory),
            preferences: preferences,
            transport: { gate }
        )
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        await model.refreshOpenView()
        XCTAssertEqual(model.answerSheet?.eventId, "e-perm")
        model.dismissQuestion()
        XCTAssertEqual(model.answerSheet?.eventId, "e-perm")
        XCTAssertEqual(model.dismissedQuestion, "e-open")
        XCTAssertEqual(preferences.dismissedQuestion(session: "a11a0001"), "e-open")
    }

    func testAClosedPermissionStaysClosedUntilTheCardIsTapped() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        let posted = Posted()
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if request.httpMethod == "POST" {
                posted.paths.append(path)
                return HostResponse(status: 204, body: Data())
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: view)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let directory = temporaryDirectory()
        let preferences = LastSessionPreference(defaults: UserDefaults(suiteName: UUID().uuidString)!)
        let model = AppModel(
            store: MemoryBaseURL(),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: ViewCache(directory: directory),
            preferences: preferences,
            transport: { gate }
        )
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        await model.refreshOpenView()
        XCTAssertEqual(model.answerSheet?.eventId, "e-perm")
        model.dismissPermission("e-perm")
        XCTAssertNotEqual(model.answerSheet?.eventId, "e-perm")
        XCTAssertEqual(model.answerSheet?.isPermission, false)
        XCTAssertEqual(preferences.dismissedPermission(session: "a11a0001"), "e-perm")
        XCTAssertTrue(posted.paths.isEmpty)
        let restored = AppModel(
            store: MemoryBaseURL(),
            drafts: DraftFiles(directory: directory.appendingPathComponent("drafts")),
            views: ViewCache(directory: directory),
            preferences: preferences,
            transport: { gate }
        )
        restored.baseURLText = sampleBase()
        await restored.connect()
        restored.open("a11a0001")
        await restored.refreshOpenView()
        XCTAssertNotEqual(restored.answerSheet?.eventId, "e-perm")
        restored.reopenPermission("e-perm")
        XCTAssertEqual(restored.answerSheet?.eventId, "e-perm")
        XCTAssertTrue(restored.answerSheet?.isPermission ?? false)
        XCTAssertNil(preferences.dismissedPermission(session: "a11a0001"))
        XCTAssertTrue(posted.paths.isEmpty)
    }

    func testAnEnhanceEditPostsRevise() async throws {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let view = try fixtureData("view.json")
        let posted = Posted()
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/answers") {
                posted.paths.append(path)
                let payload = try JSONDecoder().decode(AnswerPayload.self, from: request.httpBody ?? Data())
                posted.payloads.append(payload)
                return HostResponse(status: 204, body: Data())
            }
            if path.hasSuffix("/view") {
                return HostResponse(status: 200, body: view)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try await openSession(gate)
        let card = try XCTUnwrap(model.enhanceCard)
        XCTAssertEqual(card.eventId, "e-enhance")
        let edited = enhanceAnswer(.edit("sharper"))
        await model.answerEnhance(edited.choice, text: edited.text)
        XCTAssertEqual(
            posted.payloads,
            [AnswerPayload(id: "e-enhance", choice: "revise", text: "sharper")]
        )
        let discarded = enhanceAnswer(.discard)
        await model.answerEnhance(discarded.choice, text: discarded.text)
        XCTAssertEqual(posted.payloads.last?.choice, "discard")
        XCTAssertNil(posted.payloads.last?.text)
        XCTAssertFalse(posted.paths.contains { $0.contains("/events") })
    }

    func testANilSheetNamesAQuestionAndNotAPermission() throws {
        let ordered = try permissionBehindQuestion()
        let permission = try XCTUnwrap(AnswerSheet.front(cards: ordered.cards, dismissed: nil))
        XCTAssertNil(AnswerSheet.questionDismissed(byNil: permission))
        let questions = ordered.cards.filter { card in
            if case .permission = card.body {
                return false
            }
            return true
        }
        let question = try XCTUnwrap(AnswerSheet.front(cards: questions, dismissed: nil))
        XCTAssertEqual(AnswerSheet.questionDismissed(byNil: question), "e-open")
        XCTAssertFalse(question.isPermission)
    }

    func testAnOpenPermissionCoversTheOtherSheets() throws {
        let ordered = try permissionBehindQuestion()
        let answer = try XCTUnwrap(AnswerSheet.front(cards: ordered.cards, dismissed: nil))
        let cover = TranscriptCover.select(
            answer: answer,
            dock: true,
            thinking: true,
            context: true,
            command: true
        )
        guard case .answer(let sheet) = cover else {
            XCTFail("expected the permission sheet")
            return
        }
        XCTAssertEqual(sheet.eventId, "e-perm")
        XCTAssertEqual(
            TranscriptCover.select(answer: nil, dock: true, thinking: true, context: true, command: true),
            .dock
        )
        XCTAssertEqual(
            TranscriptCover.select(answer: nil, dock: false, thinking: true, context: true, command: true),
            .thinking
        )
        XCTAssertEqual(
            TranscriptCover.select(answer: nil, dock: false, thinking: false, context: true, command: true),
            .context
        )
        XCTAssertEqual(
            TranscriptCover.select(answer: nil, dock: false, thinking: false, context: false, command: true),
            .command
        )
        XCTAssertNil(
            TranscriptCover.select(answer: nil, dock: false, thinking: false, context: false, command: false)
        )
    }

    func testTheAnswerPathNeverReadsTheEventLog() throws {
        let request = serveRequest(
            baseURL: try XCTUnwrap(URL(string: sampleBase())),
            call: .answer(session: "a11a0001", eventId: "e-perm", choice: "deny", text: nil)
        )
        XCTAssertEqual(request.url?.path, "/v1/sessions/a11a0001/answers")
        XCTAssertFalse(request.url?.path.contains("/events") ?? true)
        let roots = [
            packageRoot().appendingPathComponent("Sources"),
            packageRoot().appendingPathComponent("App"),
        ]
        var saw = false
        for root in roots {
            let enumerator = FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil)
            while let url = enumerator?.nextObject() as? URL {
                guard url.pathExtension == "swift" else { continue }
                saw = true
                let text = try String(contentsOf: url, encoding: .utf8)
                XCTAssertFalse(text.contains("/events"), url.path)
            }
        }
        XCTAssertTrue(saw)
        let missing = #"{"id":"c5","kind":"permission","at":"2026-10-01T21:00:00.000Z","body":{"action":"Replace src/view.rs","argv":null,"decision":null,"path":"src/view.rs","timeoutSec":null}}"#
        XCTAssertThrowsError(try JSONDecoder().decode(Card.self, from: Data(missing.utf8)))
    }

    func testAnEmptyTitleUsesTheWorkspaceDirectoryName() throws {
        var session = try JSONDecoder().decode([Session].self, from: fixtureData("sessions.json"))[1]
        session.title = "  "
        let line = sessionLine(session, depth: 0)
        XCTAssertEqual(line.name, "kyotoagent")
        XCTAssertEqual(line.idPrefix, "a11a")
        XCTAssertEqual(line.badge, "question")
    }

    private func create(
        projects: Data,
        worktree: Bool,
        draft: String?,
        projectID: String?,
        profile: String? = nil,
        profiles: Data = Data("[]".utf8)
    ) async throws -> PostedSession {
        let gate = Gate()
        let sessions = try fixtureData("sessions.json")
        let created = Data("{\"id\":\"b0b0b0b0\",\"workspace\":\"/srv/host/app\",\"status\":\"idle\"}".utf8)
        gate.handler = { request in
            let path = request.url?.path ?? ""
            if request.httpMethod == "POST" {
                XCTAssertEqual(path, "/v1/sessions")
                XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
                XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
                return HostResponse(status: 201, body: created)
            }
            if path == "/v1/projects" {
                return HostResponse(status: 200, body: projects)
            }
            if path == "/v1/profiles" {
                return HostResponse(status: 200, body: profiles)
            }
            return HostResponse(status: 200, body: sessions)
        }
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        await model.loadProjects()
        if let projectID {
            model.selectedProjectID = projectID
        }
        if let draft {
            model.workspaceDraft = draft
        }
        model.useWorktree = worktree
        model.selectedProfile = profile
        XCTAssertEqual(model.profiles, (try? JSONDecoder().decode([String].self, from: profiles)) ?? [])
        let createdSession = await model.createSession()
        XCTAssertTrue(createdSession)
        XCTAssertEqual(model.selection, "b0b0b0b0")
        let call = try XCTUnwrap(gate.calls.last { $0.method == "POST" })
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: call.body ?? Data()) as? [String: Any])
        return PostedSession(
            workspace: object["workspace"] as? String,
            worktree: object["worktree"] as? Bool,
            profile: object["profile"] as? String,
            keys: Set(object.keys)
        )
    }

    private func openSession(_ gate: Gate) async throws -> AppModel {
        let model = try model(store: MemoryBaseURL(), gate: gate)
        model.baseURLText = sampleBase()
        await model.connect()
        model.open("a11a0001")
        await model.refreshOpenView()
        return model
    }

    private func questionView() throws -> Data {
        var view = try String(decoding: fixtureData("view.json"), as: UTF8.self)
        view = view.replacingOccurrences(of: "\"decision\": null", with: "\"decision\": \"deny\"")
        return Data(view.utf8)
    }

    private func permissionBehindQuestion() throws -> View {
        var view = try JSONDecoder().decode(View.self, from: fixtureData("view.json"))
        let permissionIndex = try XCTUnwrap(view.cards.firstIndex { $0.kind == .permission })
        var permission = view.cards[permissionIndex]
        if case .permission(var body) = permission.body {
            body.argv = ["cargo", "test", "--offline"]
            permission.body = .permission(body)
        }
        let questionIndex = try XCTUnwrap(view.cards.firstIndex { card in
            guard case .question(let body) = card.body else {
                return false
            }
            return body.answer == nil
        })
        let question = view.cards[questionIndex]
        view.cards.removeAll { $0.id == permission.id || $0.id == question.id }
        view.cards.append(permission)
        view.cards.append(question)
        return view
    }

    private func model(store: MemoryBaseURL, gate: Gate) throws -> AppModel {
        let directory = temporaryDirectory()
        return AppModel(
            store: store,
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

    private func fixtureData(_ name: String) throws -> Data {
        let url = iosRoot().appendingPathComponent("Fixtures").appendingPathComponent(name)
        return try Data(contentsOf: url)
    }

    private func bodyData(_ name: String) throws -> Data {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("Bodies")
            .appendingPathComponent(name)
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
}

struct RecordedCall: Equatable {
    var method: String
    var path: String
    var body: Data?
}

struct PostedSession {
    var workspace: String?
    var worktree: Bool?
    var profile: String?
    var keys: Set<String>
}

final class Flag: @unchecked Sendable {
    var on = false
}

final class Gate: HostTransport, @unchecked Sendable {
    var handler: @Sendable (URLRequest) throws -> HostResponse
    var pairHandler: (@Sendable (URLRequest) throws -> HostResponse)?
    private(set) var paths: [String] = []
    private(set) var calls: [RecordedCall] = []

    init() {
        handler = { _ in HostResponse(status: 500, body: Data()) }
    }

    func send(_ request: URLRequest) async throws -> HostResponse {
        let path = request.url?.path ?? ""
        paths.append(path)
        calls.append(RecordedCall(method: request.httpMethod ?? "GET", path: path, body: request.httpBody))
        if path == "/v1/pair" {
            return try pairHandler?(request) ?? pairingFixture(request)
        }
        return try handler(request)
    }
}

private struct TextBody: Decodable {
    var text: String
}

private final class Posted: @unchecked Sendable {
    var payloads: [AnswerPayload] = []
    var paths: [String] = []
}

private final class Served: @unchecked Sendable {
    var data: Data

    init(_ data: Data) {
        self.data = data
    }
}
