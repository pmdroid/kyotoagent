import Foundation

nonisolated public enum Status: String, Codable, Equatable, Sendable {
    case idle
    case working
    case waiting
}

nonisolated public struct AllowList: Codable, Equatable, Sendable {
    public var writePaths: [String]
    public var outsideReadPaths: [String]
    public var argv: [[String]]
    public var fetchOrigins: [String]
    public var webSearch: Bool

    public init(
        writePaths: [String] = [],
        outsideReadPaths: [String] = [],
        argv: [[String]] = [],
        fetchOrigins: [String] = [],
        webSearch: Bool = false
    ) {
        self.writePaths = writePaths
        self.outsideReadPaths = outsideReadPaths
        self.argv = argv
        self.fetchOrigins = fetchOrigins
        self.webSearch = webSearch
    }
}

nonisolated public struct Session: Codable, Equatable, Sendable {
    public var id: String
    public var workspace: String
    public var status: Status
    public var updatedAt: String
    public var createdAt: String
    public var waiting: String?
    public var pullUrl: String?
    public var compacting: Bool
    public var yolo: Bool
    public var profile: String?
    public var enhance: Bool
    public var showCloseout: Bool
    public var model: String
    public var effort: String?
    public var provider: String?
    public var title: String?
    public var project: String?
    public var parentId: String?
    public var isolation: String?
    public var hidden: Bool
    public var worktree: Bool
    public var archived: Bool
    public var archivedAt: String?
    public var allow: AllowList

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        workspace = try container.decode(String.self, forKey: .workspace)
        status = try container.decode(Status.self, forKey: .status)
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
        createdAt = try container.decode(String.self, forKey: .createdAt)
        waiting = try container.decodeIfPresent(String.self, forKey: .waiting)
        pullUrl = try container.decodeIfPresent(String.self, forKey: .pullUrl)
        compacting = try container.decodeIfPresent(Bool.self, forKey: .compacting) ?? false
        yolo = try container.decodeIfPresent(Bool.self, forKey: .yolo) ?? false
        profile = try container.decodeIfPresent(String.self, forKey: .profile)
        enhance = try container.decodeIfPresent(Bool.self, forKey: .enhance) ?? false
        showCloseout = try container.decodeIfPresent(Bool.self, forKey: .showCloseout) ?? true
        model = try container.decode(String.self, forKey: .model)
        provider = try container.decodeIfPresent(String.self, forKey: .provider)
        effort = try container.decodeIfPresent(String.self, forKey: .effort)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        project = try container.decodeIfPresent(String.self, forKey: .project)
        parentId = try container.decodeIfPresent(String.self, forKey: .parentId)
        isolation = try container.decodeIfPresent(String.self, forKey: .isolation)
        hidden = try container.decodeIfPresent(Bool.self, forKey: .hidden) ?? false
        worktree = try container.decodeIfPresent(Bool.self, forKey: .worktree) ?? false
        archived = try container.decodeIfPresent(Bool.self, forKey: .archived) ?? false
        archivedAt = try container.decodeIfPresent(String.self, forKey: .archivedAt)
        allow = try container.decode(AllowList.self, forKey: .allow)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(workspace, forKey: .workspace)
        try container.encode(status, forKey: .status)
        try container.encode(updatedAt, forKey: .updatedAt)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encodeIfPresent(waiting, forKey: .waiting)
        try container.encodeIfPresent(pullUrl, forKey: .pullUrl)
        try container.encode(compacting, forKey: .compacting)
        try container.encode(yolo, forKey: .yolo)
        try container.encodeIfPresent(profile, forKey: .profile)
        try container.encode(enhance, forKey: .enhance)
        try container.encode(showCloseout, forKey: .showCloseout)
        try container.encode(model, forKey: .model)
        try container.encodeIfPresent(effort, forKey: .effort)
        try container.encodeIfPresent(provider, forKey: .provider)
        try container.encodeIfPresent(title, forKey: .title)
        try container.encodeIfPresent(project, forKey: .project)
        try container.encodeIfPresent(parentId, forKey: .parentId)
        try container.encodeIfPresent(isolation, forKey: .isolation)
        if hidden {
            try container.encode(hidden, forKey: .hidden)
        }
        try container.encode(worktree, forKey: .worktree)
        try container.encode(archived, forKey: .archived)
        try container.encodeIfPresent(archivedAt, forKey: .archivedAt)
        try container.encode(allow, forKey: .allow)
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case workspace
        case status
        case updatedAt
        case createdAt
        case waiting
        case pullUrl
        case compacting
        case yolo
        case profile
        case enhance
        case showCloseout
        case model
        case effort
        case provider
        case title
        case project
        case parentId
        case isolation
        case hidden
        case worktree
        case archived
        case archivedAt
        case allow
    }
}
