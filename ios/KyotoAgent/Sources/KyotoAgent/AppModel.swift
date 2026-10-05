import Foundation
import Observation

@MainActor
@Observable
public final class AppModel {
    public var baseURLText: String
    public private(set) var connected = false
    public private(set) var failure: String?
    public private(set) var connecting = false
    public private(set) var pairingConfirmation: PairingInfo?
    public private(set) var sessions: [Session] = []
    public private(set) var selection: String?
    public private(set) var transcript = TranscriptWindow()
    public private(set) var transcriptFailure: String?
    public var draft = ""
    public private(set) var pastedDraft: String?
    public private(set) var notice: String?
    public private(set) var sending = false
    public private(set) var projects: [Project] = []
    public private(set) var projectsReady = false
    public private(set) var projectFailure: String?
    public var selectedProjectID: String?
    public var workspaceDraft = ""
    public var useWorktree = true
    public private(set) var profiles: [String] = []
    public var selectedProfile: String?
    public private(set) var creating = false
    public var shownPull: String?
    public private(set) var shownImage: ImageAttachment?
    private var imagesBySession: [String: [ImageAttachment]] = [:]
    public private(set) var deleteConfirm: DeleteConfirm?
    public private(set) var answering = false
    public private(set) var dismissedQuestion: String?
    public private(set) var dismissedPermission: String?
    public private(set) var openPanes: Set<SessionPane>
    public private(set) var banners: [SessionBanner] = []
    public private(set) var openedFile: FileSheet?
    public private(set) var fileFailure: String?
    public private(set) var openedTask: TaskDetail?
    public private(set) var models: [Model] = []
    public private(set) var overlay: PhoneOverlay?
    public var presentNewSession = false
    public private(set) var savingModel = false
    var modelsUnavailable = false

    private let baseURLStore: any BaseURLStoring
    private let drafts: DraftFiles
    private let views: ViewCache
    private let preferences: LastSessionPreference
    private let panes: PanePreference
    private let transport: @Sendable () -> any HostTransport
    private var client: ServeClient?
    private var connectionRequest = UUID()
    private var projectLoads = 0
    private var serverContext = UUID()
    private var requestContext = UUID()
    private var fileRequest = UUID()
    private var taskRequest = UUID()
    private var hasSessionList = false
    private var nextBanner = 0
    private var openTaskId: String?

    public init(
        store: any BaseURLStoring,
        drafts: DraftFiles,
        views: ViewCache,
        preferences: LastSessionPreference,
        transport: @escaping @Sendable () -> any HostTransport
    ) {
        baseURLStore = store
        self.drafts = drafts
        self.views = views
        self.preferences = preferences
        panes = PanePreference(defaults: preferences.defaults)
        openPanes = panes.open
        self.transport = transport
        baseURLText = (try? store.load()) ?? ""
    }

    public var sheets: SessionSheets? {
        transcript.view.map(SessionSheets.init)
    }

    public var phaseLabel: String? {
        guard displayedStatus == .working else {
            return nil
        }
        return transcript.view?.retryStatus ?? transcript.view?.action ?? transcript.view?.phase?.rawValue
    }

    public var thinkingBody: String {
        [transcript.view?.retryStatus, transcript.view?.action, transcript.view?.thinking]
            .compactMap { $0 }
            .filter { !$0.isEmpty }
            .joined(separator: "\n\n")
    }

    public var contextSheet: ContextSheet? {
        sheets?.context
    }

    public var nodes: [SessionNode] {
        nestedSessions(sessions)
    }

    public var displayedStatus: Status? {
        guard transcriptFailure == nil else { return nil }
        if let status = transcript.view?.status {
            return status
        }
        return sessions.first { $0.id == selection }?.status
    }

    public func artifacts(session: String) async throws -> [ArtifactVersion] {
        guard connected, let client else { throw HostError.transport("Connect to a server first") }
        let context = requestContext
        let server = client.baseURL
        let history = try await client.artifacts(session)
        try Swift.Task.checkCancellation()
        guard currentRequest(context, session: session, server: server) else { throw CancellationError() }
        return history
    }

    public func artifact(_ file: ArtifactFile, session: String) async throws -> Data {
        guard connected, let client else { throw HostError.transport("Connect to a server first") }
        let context = requestContext
        let server = client.baseURL
        let bytes = try await client.artifact(session, file: file)
        guard currentRequest(context, session: session, server: server) else { throw CancellationError() }
        return bytes
    }

    public func connect() async {
        pairingConfirmation = nil
        let urlText = baseURLText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let connection = PairingConnection(urlText) else {
            connected = false
            failure = "Enter a host URL or scan a pairing code"
            return
        }
        connectionRequest = UUID()
        let request = connectionRequest
        let reloadProjects = projectLoads > 0
        connecting = true
        serverContext = UUID()
        overlay = nil
        presentNewSession = false
        projects = []
        selectedProjectID = nil
        projectFailure = nil
        projectsReady = false
        requestContext = UUID()
        openedFile = nil
        fileFailure = nil
        closeTask()
        defer {
            if connectionRequest == request { connecting = false }
        }
        var next = ServeClient(baseURL: connection.baseURL, transport: transport(), token: connection.token)
        do {
            let paired = try await next.pair()
            guard connectionRequest == request else { return }
            var savedURL = urlText
            if let token = paired.access_token {
                guard var parts = URLComponents(url: connection.baseURL, resolvingAgainstBaseURL: false) else {
                    throw HostError.transport("The server returned an invalid access token.")
                }
                parts.scheme = "kyotoagent"
                parts.port = connection.baseURL.port ?? 443
                parts.queryItems = [URLQueryItem(name: "token", value: token)]
                guard let uri = parts.string, let credential = PairingConnection(uri) else {
                    throw HostError.transport("The server returned an invalid access token.")
                }
                savedURL = uri
                next.token = credential.token
                try baseURLStore.save(savedURL)
            }
            baseURLText = savedURL
            let listed = try await next.sessions()
            guard connectionRequest == request else { return }
            if paired.access_token == nil {
                try baseURLStore.save(savedURL)
            }
            client = next
            serverContext = UUID()
            models = []
            profiles = []
            requestContext = UUID()
            replaceSessions(listed)
            connected = true
            pairingConfirmation = paired
            failure = nil
            connecting = false
            if let last = preferences.id, listed.contains(where: { $0.id == last }) {
                open(last)
            }
            if reloadProjects { await loadProjects() }
        } catch let error as HostError {
            guard connectionRequest == request else { return }
            client = nil
            connected = false
            failure = error.statusAndBody
        } catch {
            guard connectionRequest == request else { return }
            client = nil
            connected = false
            failure = error.localizedDescription
        }
    }

    public func dismissPairingConfirmation() {
        pairingConfirmation = nil
    }

    public var answerSheet: AnswerSheet? {
        AnswerSheet.front(
            cards: transcript.view?.cards ?? [],
            dismissed: dismissedQuestion,
            dismissedPermission: dismissedPermission
        )
    }

    public var enhanceCard: EnhanceCard? {
        openEnhanceCard(transcript.view?.cards ?? [])
    }

    public func open(_ id: String) {
        preferences.id = id
        dismissedQuestion = preferences.dismissedQuestion(session: id)
        dismissedPermission = preferences.dismissedPermission(session: id)
        guard selection != id else {
            return
        }
        selection = id
        requestContext = UUID()
        pastedDraft = nil
        draft = drafts.load(id)
        notice = nil
        openedFile = nil
        fileFailure = nil
        closeTask()
        transcriptFailure = nil
        transcript = TranscriptWindow()
        if let cached = views.load(id) {
            _ = transcript.receive(cached)
        }
    }

    public var composerDraft: String {
        guard let pastedDraft, draft.hasPrefix(pastedDraft) else { return draft }
        return String(draft.dropFirst(pastedDraft.count))
    }

    public var pastedInput: String? {
        guard let pastedDraft, draft.hasPrefix(pastedDraft) else { return nil }
        return pastedDraft
    }

    public func updateComposerDraft(_ text: String) {
        updateDraft((pastedInput ?? "") + text)
    }

    public func removePastedInput() {
        let suffix = composerDraft
        pastedDraft = nil
        updateDraft(suffix)
    }

    public func updateDraft(_ text: String) {
        if text.count - draft.count > 2048 || text.components(separatedBy: "\n").count - draft.components(separatedBy: "\n").count > 12 {
            pastedDraft = text
        } else if let pastedDraft, !text.hasPrefix(pastedDraft) {
            self.pastedDraft = nil
        }
        draft = text
        guard let id = selection else {
            return
        }
        try? drafts.save(id, text)
    }

    public func refreshSessions() async {
        guard connected, let client else {
            return
        }
        let context = serverContext
        guard let listed = try? await client.sessions(), currentServer(context, server: client.baseURL) else {
            return
        }
        replaceSessions(listed)
    }

    public func refreshOpenView() async {
        guard connected, let id = selection, let client else {
            return
        }
        let context = requestContext
        do {
            let next = try await client.view(id)
            guard currentRequest(context, session: id, server: client.baseURL) else { return }
            transcriptFailure = nil
            if transcript.receive(next) {
                try? views.store(id, next)
            }
        } catch {
            guard currentRequest(context, session: id, server: client.baseURL) else { return }
            transcriptFailure = "Can’t refresh this chat. Retrying…"
        }
        await refreshOpenTask()
    }

    public func setPane(_ pane: SessionPane, open: Bool) {
        if open {
            openPanes.insert(pane)
        } else {
            openPanes.remove(pane)
        }
        panes.open = openPanes
    }

    public func dismissBanner(_ id: Int) {
        banners.removeAll { $0.id == id }
    }

    public func openFile(_ path: String) async {
        guard connected, let id = selection, let client else { return }
        let context = requestContext
        fileRequest = UUID()
        let request = fileRequest
        do {
            let file = try await client.file(id, path: path)
            guard currentRequest(context, session: id, server: client.baseURL), fileRequest == request else { return }
            openedFile = FileSheet(file)
            fileFailure = nil
        } catch {
            guard currentRequest(context, session: id, server: client.baseURL), fileRequest == request else { return }
            openedFile = nil
            fileFailure = (error as? HostError)?.serverMessage ?? error.localizedDescription
        }
    }

    public func openTask(_ id: String) async {
        taskRequest = UUID()
        openedTask = nil
        openTaskId = id
        await refreshOpenTask()
    }

    public func closeTask() {
        taskRequest = UUID()
        openTaskId = nil
        openedTask = nil
    }

    public var images: [ImageAttachment] {
        selection.flatMap { imagesBySession[$0] } ?? []
    }

    public func addImage(_ image: ImageAttachment, sessionID: String? = nil) {
        guard let id = sessionID ?? selection else { return }
        guard (imagesBySession[id]?.count ?? 0) < 4 else {
            imageFailure("Attach at most four images per message.")
            return
        }
        guard let bytes = image.bytes, bytes.count <= 5 * 1024 * 1024 else {
            imageFailure("Images must fit within 5 MiB.")
            return
        }
        imagesBySession[id, default: []].append(image)
        notice = nil
    }

    public func removeImage(_ imageID: String) {
        guard let id = selection else { return }
        imagesBySession[id]?.removeAll { $0.id == imageID }
    }

    public func previewImage(_ image: ImageAttachment) { shownImage = image }
    public func closeImage() { shownImage = nil }
    public func imageFailure(_ message: String) { notice = message }

    public func send() async {
        guard connected, !sending, let id = selection, let client else {
            return
        }
        let text = draft
        let attachments = images
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty else {
            return
        }
        if !attachments.isEmpty {
            await postAsk(id, client, text, images: attachments)
            return
        }
        let command = await classified(text)
        switch command {
        case .ask:
            await postAsk(id, client, text)
        case .openModel:
            clearDraft(id, text)
            await openModelList()
        case .setModel(let modelID):
            clearDraft(id, text)
            let effort = postedModel(models: models, modelID: modelID, effort: currentEffort).effort
            await saveModel(modelID, effort: effort)
        case .openEffort:
            clearDraft(id, text)
            await openEffortList()
        case .setEffort(let level):
            clearDraft(id, text)
            await saveModel(currentModelID, effort: level, provider: sessions.first { $0.id == selection }?.provider)
        case .compact:
            clearDraft(id, text)
            await compactSession()
        case .yolo(let flag):
            clearDraft(id, text)
            await applyYolo(flag)
        case .blocked:
            return
        }
    }

    public func goalCommand(_ command: String) async {
        guard connected, !sending, let id = selection, let client,
              ["status", "pause", "resume", "clear"].contains(command) else {
            return
        }
        await postAsk(id, client, "/goal " + command, preserveDraft: true)
    }

    public func helpEntries() -> [CommandEntry] {
        helpCatalog(skills: transcript.view?.skills ?? [])
    }

    private func postAsk(_ id: String, _ client: ServeClient, _ text: String, images: [ImageAttachment] = [], preserveDraft: Bool = false) async {
        sending = true
        defer { sending = false }
        let result: Result<AskAcceptance, HostError>
        do {
            result = .success(try await client.ask(id, text: text, images: images))
        } catch let error as HostError {
            result = .failure(error)
        } catch {
            result = .failure(.transport(error.localizedDescription))
        }
        if case .success = result {
            let sentIDs = Set(images.map(\.id))
            imagesBySession[id]?.removeAll { sentIDs.contains($0.id) }
            notice = nil
            if !preserveDraft && draft == text {
                draft = ""
                pastedDraft = nil
                try? drafts.save(id, "")
            }
        } else if case .failure(let error) = result {
            notice = error.serverMessage
        }
        await refreshSessions()
        await refreshOpenView()
    }

    func clearDraft(_ id: String, _ text: String) {
        guard draft == text else {
            return
        }
        draft = ""
        pastedDraft = nil
        try? drafts.save(id, "")
    }

    public var canCreate: Bool {
        guard projectsReady, projectFailure == nil, !creating else {
            return false
        }
        if projects.isEmpty {
            return !workspaceDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
        return projects.contains { $0.id == selectedProjectID }
    }

    public func loadProjects() async {
        projectLoads += 1
        defer { projectLoads -= 1 }
        projectsReady = false
        workspaceDraft = ""
        useWorktree = true
        selectedProfile = nil
        profiles = []
        guard connected, let client else {
            projects = []
            selectedProjectID = nil
            projectFailure = "Enter a host URL"
            projectsReady = true
            return
        }
        let context = serverContext
        do {
            let listed = try await client.projects()
            let availableProfiles = (try? await client.profiles()) ?? []
            guard currentServer(context, server: client.baseURL) else { return }
            projects = listed
            projectFailure = nil
            selectedProjectID = listed.first?.id
            profiles = availableProfiles
        } catch let error as HostError {
            guard currentServer(context, server: client.baseURL) else { return }
            projects = []
            selectedProjectID = nil
            projectFailure = error.serverMessage
        } catch {
            guard currentServer(context, server: client.baseURL) else { return }
            projects = []
            selectedProjectID = nil
            projectFailure = error.localizedDescription
        }
        projectsReady = true
    }

    @discardableResult
    public func createSession() async -> Bool {
        guard connected, let client, canCreate else {
            return false
        }
        let workspace: String
        if projects.isEmpty {
            workspace = workspaceDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        } else if let project = projects.first(where: { $0.id == selectedProjectID }) {
            workspace = project.path
        } else {
            return false
        }
        creating = true
        defer { creating = false }
        do {
            let created = try await client.createSession(
                workspace: workspace,
                worktree: useWorktree,
                profile: createProfileField(selectedProfile)
            )
            notice = nil
            await refreshSessions()
            open(created.id)
            return true
        } catch let error as HostError {
            notice = error.serverMessage
            return false
        } catch {
            notice = error.localizedDescription
            return false
        }
    }

    public var deleteShortcutBlocked: Bool {
        deleteConfirm != nil
            || overlay != nil
            || presentNewSession
            || shownPull != nil
            || openedFile != nil
            || openedTask != nil
            || answerSheet != nil
    }

    public func requestDelete(_ id: String) {
        guard let session = sessions.first(where: { $0.id == id }) else {
            return
        }
        deleteConfirm = DeleteConfirm(
            id: id,
            workspace: session.workspace,
            removesDirectory: session.worktree
        )
    }

    public func cancelDelete() {
        deleteConfirm = nil
    }

    public func confirmDelete(deleteWorkspace: Bool = false) async {
        guard let confirm = deleteConfirm else {
            return
        }
        deleteConfirm = nil
        await closeSession(confirm.id, deleteWorkspace: deleteWorkspace && confirm.removesDirectory)
    }

    public func pressDelete() {
        guard !deleteShortcutBlocked, let id = selection else {
            return
        }
        requestDelete(id)
    }

    public func closeSession(_ id: String, deleteWorkspace: Bool = false) async {
        guard connected, let client else {
            return
        }
        do {
            try await client.deleteSession(id, deleteWorkspace: deleteWorkspace)
        } catch let error as HostError {
            notice = error.serverMessage
            return
        } catch {
            notice = error.localizedDescription
            return
        }
        notice = nil
        if selection == id {
            selection = nil
            requestContext = UUID()
            draft = ""
            pastedDraft = nil
            transcriptFailure = nil
            transcript = TranscriptWindow()
        }
        if preferences.id == id {
            preferences.id = nil
        }
        sessions.removeAll { $0.id == id }
        await refreshSessions()
    }

    public func openPull(_ session: Session) {
        guard let pull = session.pullUrl, safariURL(pull) != nil else {
            shownPull = nil
            return
        }
        shownPull = pull
    }

    public func cancelTurn() async {
        guard connected, let id = selection, let client else {
            return
        }
        do {
            try await client.cancel(id)
        } catch let error as HostError {
            notice = error.serverMessage
        } catch {
            notice = error.localizedDescription
        }
        await refreshSessions()
        await refreshOpenView()
    }

    public func answer(_ choice: String) async {
        guard connected, !answering, !choice.isEmpty, let id = selection, let client else {
            return
        }
        guard let eventId = answerSheet?.eventId else {
            return
        }
        answering = true
        defer { answering = false }
        do {
            try await client.answer(id, eventId: eventId, choice: choice)
            notice = nil
        } catch let error as HostError {
            notice = error.serverMessage
        } catch {
            notice = error.localizedDescription
        }
        await refreshSessions()
        await refreshOpenView()
    }

    public func dismissQuestion() {
        guard let id = selection, case .question(let question) = answerSheet else {
            return
        }
        preferences.dismissQuestion(question.eventId, session: id)
        dismissedQuestion = question.eventId
    }

    public func dismissPermission(_ eventId: String) {
        guard let id = selection else {
            return
        }
        preferences.dismissPermission(eventId, session: id)
        dismissedPermission = eventId
    }

    public func reopenPermission(_ eventId: String) {
        guard let id = selection, dismissedPermission == eventId else {
            return
        }
        preferences.clearDismissedPermission(session: id)
        dismissedPermission = nil
    }

    public func answerEnhance(_ choice: String, text: String?) async {
        guard connected, !answering, let id = selection, let client, let card = enhanceCard else {
            return
        }
        answering = true
        defer { answering = false }
        do {
            try await client.answer(id, eventId: card.eventId, choice: choice, text: text)
            notice = nil
        } catch let error as HostError {
            notice = error.serverMessage
        } catch {
            notice = error.localizedDescription
        }
        await refreshSessions()
        await refreshOpenView()
    }

    private func replaceSessions(_ listed: [Session]) {
        if hasSessionList {
            appendBanners(sessionTransitions(previous: sessions, next: listed, openId: selection))
        }
        sessions = listed
        hasSessionList = true
    }

    private func appendBanners(_ raised: [SessionTransition]) {
        for transition in raised {
            banners.append(SessionBanner(id: nextBanner, sessionId: transition.sessionId, phrase: transition.phrase))
            nextBanner += 1
        }
    }

    private func currentServer(_ context: UUID, server: URL) -> Bool {
        serverContext == context && connected && !connecting && client?.baseURL == server && !Swift.Task.isCancelled
    }

    private func currentRequest(_ context: UUID, session: String, server: URL) -> Bool {
        requestContext == context && connected && !connecting && selection == session && client?.baseURL == server && !Swift.Task.isCancelled
    }

    private func refreshOpenTask() async {
        guard connected, let sessionId = selection, let taskId = openTaskId, let client else { return }
        let context = requestContext
        let request = taskRequest
        guard let task = try? await client.task(sessionId, taskId) else { return }
        guard currentRequest(context, session: sessionId, server: client.baseURL), openTaskId == taskId, taskRequest == request else { return }
        openedTask = TaskDetail(task)
    }

    public func openPalette() {
        presentNewSession = false
        overlay = .palette
    }

    public func dismissOverlay() {
        overlay = nil
        presentNewSession = false
    }

    public func openModelList() async {
        let context = serverContext
        await reloadModels()
        guard context == serverContext, !Swift.Task.isCancelled else { return }
        overlay = .model
    }

    public func openEffortList() async {
        let context = serverContext
        await reloadModels()
        guard context == serverContext, !Swift.Task.isCancelled else { return }
        overlay = .effort
    }

    public func openProfileList() async {
        let context = serverContext
        await reloadProfiles()
        guard context == serverContext, !Swift.Task.isCancelled else { return }
        overlay = .profile
    }

    public func applyProfile(_ choice: String) async {
        guard connected, let id = selection, let client else {
            return
        }
        do {
            try await client.setProfile(id, profile: liveProfileField(choice))
            notice = nil
            dismissOverlay()
        } catch let error as HostError {
            notice = error.serverMessage
        } catch {
            notice = error.localizedDescription
        }
        await refreshSessions()
        await refreshOpenView()
    }

    @discardableResult
    public func runPalette(_ kind: CommandKind) async -> PaletteResult {
        switch kind {
        case .newSession:
            presentNewSession = true
            return .showNewSession
        case .help:
            return .showHelp
        case .openModel:
            await openModelList()
            return .finished
        case .openEffort:
            await openEffortList()
            return .finished
        case .compact:
            await compactSession()
            dismissOverlay()
            return .finished
        case .yolo:
            await applyYolo(.toggle)
            dismissOverlay()
            return .finished
        case .profile:
            await openProfileList()
            return .finished
        case .cancel:
            await cancelTurn()
            dismissOverlay()
            return .finished
        case .closeSession:
            let id = selection
            dismissOverlay()
            if let id {
                requestDelete(id)
            }
            return .finished
        case .skill(let name):
            updateDraft(filledSkill(name))
            dismissOverlay()
            return .finished
        }
    }

    public func compactSession() async {
        guard connected, let id = selection, let client else {
            return
        }
        do {
            try await client.compact(id)
            notice = nil
        } catch let error as HostError {
            notice = error.serverMessage
        } catch {
            notice = error.localizedDescription
        }
        await refreshSessions()
        await refreshOpenView()
    }

    public func applyYolo(_ flag: YoloFlag) async {
        let next: Bool
        switch flag {
        case .toggle:
            next = !currentYolo
        case .on:
            next = true
        case .off:
            next = false
        }
        guard connected, let id = selection, let client else {
            return
        }
        do {
            try await client.setYolo(id, yolo: next)
            notice = nil
        } catch let error as HostError {
            notice = error.serverMessage
        } catch {
            notice = error.localizedDescription
        }
        await refreshSessions()
        await refreshOpenView()
    }

    public func saveModel(_ modelID: String, effort: String?, provider: String? = nil, saveAsDefault: Bool = false, dismiss: Bool = true) async {
        guard !savingModel else { return }
        guard connected, let id = selection, let client else {
            return
        }
        let choices = provider.map { value in
            let matching = models.filter { $0.provider == value }
            return matching.contains { $0.id == modelID } ? matching : models.filter { $0.provider == nil }
        } ?? models
        let payload = postedModel(models: choices, modelID: modelID, effort: effort)
        savingModel = true
        defer { savingModel = false }
        do {
            try await client.setModel(id, model: payload.model, effort: payload.effort, provider: provider, saveAsDefault: saveAsDefault)
            if let index = sessions.firstIndex(where: { $0.id == id }) {
                sessions[index].model = payload.model
                sessions[index].effort = payload.effort
                sessions[index].provider = provider
            }
            notice = nil
            if dismiss { dismissOverlay() }
        } catch let error as HostError {
            if case .status(404, _) = error, !saveAsDefault {
                notice = "Update the server to switch models for this session."
            } else {
                notice = error.serverMessage
            }
        } catch {
            notice = error.localizedDescription
        }
        await refreshSessions()
        await refreshOpenView()
    }

    func classified(_ text: String) async -> SlashCommand {
        let name = slashParts(text)?.name ?? ""
        if name == "model" || name == "effort" {
            await reloadModels()
        }
        if name == "effort", modelsUnavailable, slashParts(text)?.argument != nil {
            return .blocked
        }
        let efforts = effortChoices(for: currentModelID, in: models)
        return slashCommand(text, efforts: efforts)
    }

    func reloadProfiles() async {
        guard connected, let client else {
            profiles = []
            return
        }
        let context = serverContext
        do {
            let listed = try await client.profiles()
            guard currentServer(context, server: client.baseURL) else { return }
            profiles = listed
        } catch let error as HostError {
            guard currentServer(context, server: client.baseURL) else { return }
            profiles = []
            notice = error.serverMessage
        } catch {
            guard currentServer(context, server: client.baseURL) else { return }
            profiles = []
            notice = error.localizedDescription
        }
    }

    func reloadModels() async {
        guard connected, let client else {
            modelsUnavailable = true
            return
        }
        let context = serverContext
        do {
            let listed = try await client.models()
            guard currentServer(context, server: client.baseURL) else { return }
            models = listed
            modelsUnavailable = false
        } catch let error as HostError {
            guard currentServer(context, server: client.baseURL) else { return }
            modelsUnavailable = true
            notice = error.serverMessage
        } catch {
            guard currentServer(context, server: client.baseURL) else { return }
            modelsUnavailable = true
            notice = error.localizedDescription
        }
    }

    private var currentModelID: String {
        sessions.first { $0.id == selection }?.model ?? ""
    }

    private var currentEffort: String? {
        sessions.first { $0.id == selection }?.effort
    }

    private var currentYolo: Bool {
        sessions.first { $0.id == selection }?.yolo ?? false
    }
}

nonisolated public struct SessionBanner: Equatable, Sendable, Identifiable {
    public var id: Int
    public var sessionId: String
    public var phrase: String
}

func serveBase(_ text: String) -> URL? {
    PairingConnection(text)?.baseURL
}
