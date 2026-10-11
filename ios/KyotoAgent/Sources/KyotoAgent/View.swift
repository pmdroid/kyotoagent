import Foundation

nonisolated public struct Skill: Codable, Equatable, Sendable {
    public var name: String
    public var description: String
    public var disable_model_invocation: Bool
    public var user_invocable: Bool
    public var path: String
}

nonisolated public enum TodoStatus: String, Codable, Equatable, Sendable {
    case pending
    case in_progress
    case done
}

nonisolated public struct Todo: Codable, Equatable, Sendable {
    public var id: String
    public var title: String
    public var status: TodoStatus
    public var description: String?
    public var files: [String]?
    public var links: [String]?
}

nonisolated public struct Schedule: Codable, Equatable, Sendable {
    public var id: String
    public var note: String
    public var dueAt: String
    public var remainingMin: Int
}

nonisolated public enum Phase: String, Codable, Equatable, Sendable {
    case thinking
    case tool
}

nonisolated public enum CloseoutStatus: String, Codable, Equatable, Sendable {
    case notRequired = "not_required"
    case missing
    case running
    case passed
    case failed
}

nonisolated public struct CloseoutCheck: Codable, Equatable, Sendable {
    public var runs: [CloseoutAttempt]? = nil
    public var id: String
    public var kind: String
    public var hint: String
    public var status: CloseoutStatus
    public var required: Bool? = nil
    public var exit: Int?
    public var attempt: Int?
    public var tail: String?
}

nonisolated public struct CloseoutAttempt: Codable, Equatable, Sendable {
    public var passed: Bool? = nil
    public var id: String
    public var attempt: Int
    public var exit: Int
    public var tail: String
    public var timed_out: Bool?
    public var truncated: Bool?
    public var transcript: ArtifactFile?
}

nonisolated public func closeoutAttemptSummary(_ run: CloseoutAttempt) -> String {
    let status = run.timed_out == true ? "timed out" : (run.passed ?? (run.exit == 0)) ? "passed" : "failed"
    return "Attempt \(run.attempt) · \(status) · exit \(run.exit)"
}

nonisolated public enum BucketId: String, Codable, Equatable, Sendable {
    case system
    case tools
    case skills
    case messages
    case free
}

nonisolated public struct ContextBucket: Codable, Equatable, Sendable {
    public var id: BucketId
    public var tokens: Int?
}

nonisolated public struct ContextUsage: Codable, Equatable, Sendable {
    public var reported_prompt_tokens: Int? = nil
    public var used: Int
    public var window: Int?
    public var percent: Int?
    public var buckets: [ContextBucket]
}

nonisolated public struct View: Codable, Equatable, Sendable {
    public var status: Status
    public var cards: [Card]
    public var revision: Int
    public var skills: [Skill]
    public var todos: [Todo]
    public var tasks: [Task]
    public var schedules: [Schedule]
    public var phase: Phase?
    public var action: String?
    public var retryStatus: String?
    public var thinking: String?
    public var queue: [String]
    public var allow: AllowList
    public var closeout: [CloseoutCheck]
    public var closeoutBypassed: Bool
    public var context: ContextUsage?
    public var goal: Goal?

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        status = try container.decode(Status.self, forKey: .status)
        cards = try container.decode([Card].self, forKey: .cards)
        revision = try container.decode(Int.self, forKey: .revision)
        skills = try container.decodeIfPresent([Skill].self, forKey: .skills) ?? []
        todos = try container.decodeIfPresent([Todo].self, forKey: .todos) ?? []
        tasks = try container.decodeIfPresent([Task].self, forKey: .tasks) ?? []
        schedules = try container.decodeIfPresent([Schedule].self, forKey: .schedules) ?? []
        phase = try container.decodeIfPresent(Phase.self, forKey: .phase)
        action = try container.decodeIfPresent(String.self, forKey: .action)
        retryStatus = try container.decodeIfPresent(String.self, forKey: .retryStatus)
        thinking = try container.decodeIfPresent(String.self, forKey: .thinking)
        queue = try container.decodeIfPresent([String].self, forKey: .queue) ?? []
        allow = try container.decodeIfPresent(AllowList.self, forKey: .allow) ?? AllowList()
        closeout = try container.decodeIfPresent([CloseoutCheck].self, forKey: .closeout) ?? []
        closeoutBypassed = try container.decodeIfPresent(Bool.self, forKey: .closeoutBypassed) ?? false
        context = try container.decodeIfPresent(ContextUsage.self, forKey: .context)
        goal = try container.decodeIfPresent(Goal.self, forKey: .goal)
    }

    nonisolated public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(status, forKey: .status)
        try container.encode(cards, forKey: .cards)
        try container.encode(revision, forKey: .revision)
        try container.encode(skills, forKey: .skills)
        try container.encode(todos, forKey: .todos)
        try container.encode(tasks, forKey: .tasks)
        try container.encode(schedules, forKey: .schedules)
        try container.encodeIfPresent(phase, forKey: .phase)
        try container.encodeIfPresent(action, forKey: .action)
        try container.encodeIfPresent(retryStatus, forKey: .retryStatus)
        try container.encodeIfPresent(thinking, forKey: .thinking)
        try container.encode(queue, forKey: .queue)
        try container.encode(allow, forKey: .allow)
        try container.encode(closeout, forKey: .closeout)
        try container.encode(closeoutBypassed, forKey: .closeoutBypassed)
        try container.encodeIfPresent(context, forKey: .context)
        try container.encodeIfPresent(goal, forKey: .goal)
    }

    private enum CodingKeys: String, CodingKey {
        case status
        case cards
        case revision
        case skills
        case todos
        case tasks
        case schedules
        case phase
        case action
        case retryStatus = "retry_status"
        case thinking
        case queue
        case allow
        case closeout
        case closeoutBypassed = "closeout_bypassed"
        case context
        case goal
    }
}
