import Foundation

nonisolated public enum SessionPane: String, Codable, Equatable, Sendable, CaseIterable {
    case todos
    case closeout
    case tasks
    case schedules
}

nonisolated public struct PanePreference {
    public var defaults: UserDefaults
    public var key: String

    public init(defaults: UserDefaults, key: String = "openPanes") {
        self.defaults = defaults
        self.key = key
    }

    public var open: Set<SessionPane> {
        get {
            guard let raw = defaults.string(forKey: key),
                  let data = raw.data(using: .utf8),
                  let names = try? JSONDecoder().decode([String].self, from: data)
            else {
                return []
            }
            return Set(names.compactMap(SessionPane.init(rawValue:)))
        }
        nonmutating set {
            let names = newValue.map(\.rawValue).sorted()
            guard let data = try? JSONEncoder().encode(names),
                  let raw = String(data: data, encoding: .utf8)
            else {
                return
            }
            defaults.set(raw, forKey: key)
        }
    }
}

nonisolated public struct TodoRow: Equatable, Sendable, Identifiable {
    public var id: String
    public var title: String
    public var status: TodoStatus
    public var description: String?
    public var files: [String]
    public var links: [String]

    public init(_ todo: Todo) {
        id = todo.id
        title = todo.title
        status = todo.status
        description = todo.description
        files = todo.files ?? []
        links = todo.links ?? []
    }
}

nonisolated public struct CloseoutRow: Equatable, Sendable, Identifiable {
    public var runs: [CloseoutAttempt]
    public var id: String
    public var kind: String
    public var status: CloseoutStatus
    public var required: Bool
    public var attempt: Int?
    public var exit: Int?
    public var tail: String

    public init(_ check: CloseoutCheck) {
        runs = check.runs ?? []
        id = check.id
        kind = check.kind
        status = check.status
        required = check.required ?? (check.status != .notRequired)
        attempt = check.attempt
        exit = check.exit
        tail = check.tail ?? ""
    }
}

nonisolated public func closeoutSummary(_ row: CloseoutRow) -> String {
    if row.status == .notRequired {
        return [row.id, row.kind, closeoutRequirement(row)].joined(separator: " · ")
    }
    let attempt = row.attempt.map(String.init) ?? "—"
    let exit = row.exit.map(String.init) ?? "—"
    return [row.id, row.kind, closeoutRequirement(row), row.status.rawValue, attempt, exit].joined(separator: " · ")
}

nonisolated public func closeoutRequirement(_ row: CloseoutRow) -> String {
    if row.required {
        return "Required for this turn"
    }
    return row.status == .notRequired ? "Not required; won't run" : "Not required"
}

nonisolated public struct TaskLine: Equatable, Sendable, Identifiable {
    public var id: String
    public var argv: [String]
    public var state: TaskState

    public init(_ task: Task) {
        id = task.id
        argv = task.argv
        state = task.state
    }
}

nonisolated public struct TaskDetail: Equatable, Sendable {
    public var id: String
    public var argv: [String]
    public var state: TaskState
    public var exit: Int?
    public var tail: String

    public init(_ task: Task) {
        id = task.id
        argv = task.argv
        state = task.state
        exit = task.exit
        tail = task.tail ?? ""
    }
}

nonisolated public struct ScheduleRow: Equatable, Sendable, Identifiable {
    public var id: String
    public var note: String
    public var remainingMin: Int

    public init(_ schedule: Schedule) {
        id = schedule.id
        note = schedule.note
        remainingMin = schedule.remainingMin
    }
}

nonisolated public struct FileSheet: Equatable, Sendable {
    public var path: String
    public var text: String
    public var truncated: Bool

    public init(_ file: File) {
        path = file.path
        text = file.text
        truncated = file.truncated
    }
}

nonisolated public struct ContextLine: Equatable, Sendable, Identifiable {
    public var id: BucketId
    public var tokens: Int?
}

nonisolated public struct ContextSheet: Equatable, Sendable {
    public var reportedPromptTokens: Int?
    public var used: Int
    public var window: Int?
    public var percent: Int
    public var buckets: [ContextLine]

    public init?(_ usage: ContextUsage) {
        guard let percent = usage.percent else {
            return nil
        }
        reportedPromptTokens = usage.reported_prompt_tokens
        used = usage.used
        window = usage.window
        self.percent = percent
        var seen = Set<BucketId>()
        var tokens: [BucketId: Int?] = [:]
        for bucket in usage.buckets where seen.insert(bucket.id).inserted {
            tokens[bucket.id] = bucket.tokens
        }
        buckets = [BucketId.system, .tools, .skills, .messages, .free].map { id in
            ContextLine(id: id, tokens: tokens[id] ?? nil)
        }
    }
}

nonisolated public struct SessionSheets: Equatable, Sendable {
    public var todos: [TodoRow]
    public var closeout: [CloseoutRow]
    public var tasks: [TaskLine]
    public var schedules: [ScheduleRow]
    public var phase: Phase?
    public var thinking: String
    public var context: ContextSheet?

    public init(_ view: View) {
        todos = view.todos.map(TodoRow.init)
        closeout = view.closeout.map(CloseoutRow.init)
        tasks = view.tasks.map(TaskLine.init)
        schedules = view.schedules.map(ScheduleRow.init)
        phase = view.phase
        thinking = view.thinking ?? ""
        context = view.context.flatMap(ContextSheet.init)
    }
}

nonisolated public struct SessionTransition: Equatable, Sendable {
    public var sessionId: String
    public var phrase: String
}

nonisolated public func bannerPhrase(previous: Status?, status: Status, waiting: String?) -> String? {
    switch (previous, status) {
    case (nil, .waiting), (.working, .waiting), (.idle, .waiting):
        if waiting == "question" {
            return "needs a question"
        }
        if waiting == "enhance" {
            return "needs a prompt"
        }
        return "needs a permission"
    case (.working, .idle), (.waiting, .idle):
        return "finished"
    default:
        return nil
    }
}

nonisolated public func sessionTransitions(
    previous: [Session],
    next: [Session],
    openId: String?
) -> [SessionTransition] {
    let prior = Dictionary(previous.map { ($0.id, $0.status) }, uniquingKeysWith: { _, latest in latest })
    var raised: [SessionTransition] = []
    for session in next where session.id != openId {
        guard let phrase = bannerPhrase(previous: prior[session.id], status: session.status, waiting: session.waiting) else {
            continue
        }
        raised.append(SessionTransition(sessionId: session.id, phrase: phrase))
    }
    return raised
}

nonisolated public func sessionListPolls(connected: Bool, sceneIsActive: Bool) -> Bool {
    connected && sceneIsActive
}

nonisolated public func openViewPolls(connected: Bool, sceneIsActive: Bool, sessionOpen: Bool) -> Bool {
    connected && sceneIsActive && sessionOpen
}

nonisolated public enum ColumnWidth: Equatable, Sendable {
    case phone
    case pad
}

nonisolated public struct ColumnPlan: Equatable, Sendable {
    public var columns: Int
    public var paneColumn: Bool

    public init(columns: Int, paneColumn: Bool) {
        self.columns = columns
        self.paneColumn = paneColumn
    }
}

nonisolated public func panesHaveRows(_ sheets: SessionSheets?) -> Bool {
    guard let sheets else {
        return false
    }
    return !sheets.todos.isEmpty || !sheets.closeout.isEmpty || !sheets.tasks.isEmpty || !sheets.schedules.isEmpty
}

nonisolated public func columnPlan(width: ColumnWidth, panesHaveRows: Bool) -> ColumnPlan {
    switch width {
    case .phone:
        return ColumnPlan(columns: 1, paneColumn: false)
    case .pad:
        if panesHaveRows {
            return ColumnPlan(columns: 3, paneColumn: true)
        }
        return ColumnPlan(columns: 2, paneColumn: false)
    }
}

nonisolated public enum ComposerChip: String, Equatable, Sendable, CaseIterable {
    case todos
    case closeout
    case tasks
    case more
}

nonisolated public func composerChips(width: ColumnWidth, panesHaveRows: Bool) -> [ComposerChip] {
    switch (width, panesHaveRows) {
    case (.phone, _), (.pad, _):
        return []
    }
}

nonisolated public enum TranscriptPointer: Equatable, Sendable {
    case cardList
    case control
    case link
    case field
    case scroll
}

nonisolated public func transcriptRequestsKeyboardDismissal(_ pointer: TranscriptPointer) -> Bool {
    switch pointer {
    case .cardList, .scroll:
        return true
    case .control, .link, .field:
        return false
    }
}

nonisolated public enum TranscriptDoubleTap: Equatable, Sendable {
    case openPalette
    case ignore
}

nonisolated public func transcriptDoubleTap(_ pointer: TranscriptPointer) -> TranscriptDoubleTap {
    switch pointer {
    case .cardList, .scroll, .link:
        return .openPalette
    case .field, .control:
        return .ignore
    }
}

nonisolated public func transcriptPointerForCardTap(holdsLink: Bool) -> TranscriptPointer {
    holdsLink ? .link : .cardList
}

nonisolated public struct TranscriptBar: Equatable, Sendable {
    public var title: String
    public var onBackLine: Bool
    public var largeTitle: String?
    public var under: [String]
}

nonisolated public func transcriptBar(
    name: String,
    phase: String?,
    percent: Int?,
    yolo: String?,
    profile: String? = nil,
    badge: String?
) -> TranscriptBar {
    let title = name.trimmingCharacters(in: .whitespacesAndNewlines)
    return TranscriptBar(
        title: title,
        onBackLine: !title.isEmpty,
        largeTitle: nil,
        under: transcriptUnder(phase: phase, percent: percent, yolo: yolo, profile: profile, badge: badge)
    )
}

nonisolated private func transcriptUnder(
    phase: String?,
    percent: Int?,
    yolo: String?,
    profile: String?,
    badge: String?
) -> [String] {
    var line: [String] = []
    if let phase = filled(phase) {
        line.append(phase)
    }
    if let percent {
        line.append(String(percent) + "%")
    }
    if let yolo = filled(yolo) {
        line.append(yolo)
    }
    if let profile = filled(profile) {
        line.append(profile)
    }
    if let badge = filled(badge) {
        line.append(badge)
    }
    return line
}

nonisolated private func filled(_ value: String?) -> String? {
    guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else {
        return nil
    }
    return trimmed
}

nonisolated public enum CompactColumn: Equatable, Sendable {
    case list
    case detail
}

nonisolated public enum SessionRowTap: Equatable, Sendable {
    case session(String)
    case pull
}

nonisolated public func compactColumn(after tap: SessionRowTap, selection: String?) -> CompactColumn {
    switch tap {
    case .pull:
        return .list
    case .session(let id) where id == selection:
        return .detail
    case .session(let id) where !id.isEmpty:
        return .detail
    case .session:
        return .list
    }
}

nonisolated public enum CompactSlot: Equatable, Sendable {
    case sidebar
    case content
    case detail
}

nonisolated public func preferredCompactSlot(showTranscript: Bool, paneColumn: Bool) -> CompactSlot {
    if !showTranscript {
        return .sidebar
    }
    if paneColumn {
        return .content
    }
    return .detail
}
