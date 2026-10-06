import Foundation

nonisolated public struct SessionNode: Equatable, Sendable, Identifiable {
    public var session: Session
    public var depth: Int
    public var id: String { session.id }

    public init(session: Session, depth: Int) {
        self.session = session
        self.depth = depth
    }
}

nonisolated public struct SessionLine: Equatable, Sendable {
    public var name: String
    public var idPrefix: String
    public var status: Status
    public var waiting: String?
    public var marks: [String]
    public var depth: Int

    public var detail: String {
        var parts = [idPrefix]
        parts.append(contentsOf: marks)
        parts.append(status.rawValue)
        return parts.joined(separator: " · ")
    }

    public var badge: String {
        if marks.contains("archived") {
            return "archived"
        }
        if status == .waiting, let waiting, !waiting.isEmpty {
            return waiting
        }
        return status.rawValue
    }
}

nonisolated public enum SessionListFilter: String, CaseIterable, Equatable, Sendable, Identifiable {
    case all
    case running
    case questions
    case archived

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .all:
            return "All"
        case .running:
            return "Running"
        case .questions:
            return "Questions"
        case .archived:
            return "Archived"
        }
    }

    public func next() -> SessionListFilter {
        let values = Self.allCases
        let index = values.firstIndex(of: self) ?? 0
        return values[(index + 1) % values.count]
    }
}

nonisolated public func sessionMatches(_ session: Session, filter: SessionListFilter) -> Bool {
    switch filter {
    case .all:
        return !session.archived
    case .running:
        return !session.archived && session.status == .working
    case .questions:
        return !session.archived && session.status == .waiting && session.waiting == "question"
    case .archived:
        return session.archived
    }
}

nonisolated public func filteredSessions(_ rows: [Session], filter: SessionListFilter) -> [Session] {
    rows.filter { sessionMatches($0, filter: filter) }
}

nonisolated public func nestedSessions(_ rows: [Session]) -> [SessionNode] {
    let rows = rows.filter { !$0.hidden }
    let ids = Set(rows.map(\.id))
    var out: [SessionNode] = []
    var seen = Set<String>()
    for row in rows {
        let nested = row.parentId.map { ids.contains($0) } ?? false
        if nested || !seen.insert(row.id).inserted {
            continue
        }
        out.append(SessionNode(session: row, depth: 0))
        for child in rows where child.parentId == row.id && seen.insert(child.id).inserted {
            out.append(SessionNode(session: child, depth: 1))
        }
    }
    for row in rows where seen.insert(row.id).inserted {
        out.append(SessionNode(session: row, depth: 0))
    }
    return out
}

nonisolated public func sessionLine(_ session: Session, depth: Int) -> SessionLine {
    var marks: [String] = []
    if depth > 0 {
        marks.append("child")
    }
    if session.yolo {
        marks.append("yolo")
    }
    if session.compacting {
        marks.append("compacting")
    }
    if session.archived {
        marks.append("archived")
    }
    if !session.model.isEmpty {
        marks.append(session.model)
    }
    if let effort = session.effort?.trimmingCharacters(in: .whitespacesAndNewlines), !effort.isEmpty {
        marks.append(effort)
    }
    let waiting = session.status == .waiting ? session.waiting : nil
    return SessionLine(
        name: sessionName(session),
        idPrefix: String(session.id.prefix(4)),
        status: session.status,
        waiting: waiting,
        marks: marks,
        depth: depth
    )
}

nonisolated public func headerYoloWord(_ session: Session) -> String? {
    session.yolo ? "yolo" : nil
}

nonisolated public func headerProfileWord(_ session: Session) -> String? {
    guard let name = session.profile?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty else {
        return nil
    }
    return name
}

nonisolated public func sessionName(_ session: Session) -> String {
    if let title = session.title?.trimmingCharacters(in: .whitespacesAndNewlines), !title.isEmpty {
        return title
    }
    let name = directoryName(session.workspace)
    if !name.isEmpty {
        return name
    }
    return session.id
}

nonisolated func directoryName(_ path: String) -> String {
    let trimmed = path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
    guard let last = trimmed.split(separator: "/").last else {
        return ""
    }
    return String(last)
}

nonisolated public struct DeleteConfirm: Equatable, Sendable {
    public var id: String
    public var workspace: String
    public var removesDirectory: Bool

    public init(id: String, workspace: String, removesDirectory: Bool) {
        self.id = id
        self.workspace = workspace
        self.removesDirectory = removesDirectory
    }
}

nonisolated public let deletePrompt = "Delete this session?"
nonisolated public let deleteDirectory = "That directory will be removed."

nonisolated public func deleteConfirmLines(removesDirectory: Bool, workspace: String) -> [String] {
    var lines = [deletePrompt]
    if removesDirectory {
        lines.append(workspace)
        lines.append(deleteDirectory)
    }
    return lines
}

nonisolated public func safariURL(_ text: String) -> URL? {
    guard let url = URL(string: text) else {
        return nil
    }
    guard let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http" else {
        return nil
    }
    guard url.host != nil else {
        return nil
    }
    return url
}

nonisolated public func hostLabel(_ baseURL: String) -> String {
    guard let host = URL(string: baseURL)?.host, !host.isEmpty else {
        return baseURL
    }
    return host
}
