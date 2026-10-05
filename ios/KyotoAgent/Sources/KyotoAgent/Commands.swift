import Foundation

public enum YoloFlag: Equatable, Sendable {
    case toggle
    case on
    case off
}

public enum SlashCommand: Equatable, Sendable {
    case openModel
    case setModel(String)
    case openEffort
    case setEffort(String)
    case compact
    case yolo(YoloFlag)
    case ask
    case blocked
}

public enum CommandKind: Equatable, Sendable {
    case newSession
    case openModel
    case openEffort
    case compact
    case yolo
    case profile
    case cancel
    case closeSession
    case archiveSession
    case unarchiveSession
    case help
    case skill(String)
}

public struct CommandEntry: Equatable, Sendable, Identifiable {
    public var id: String
    public var title: String
    public var hint: String
    public var kind: CommandKind

    public init(id: String, title: String, hint: String, kind: CommandKind) {
        self.id = id
        self.title = title
        self.hint = hint
        self.kind = kind
    }
}

nonisolated public struct ModelPayload: Codable, Equatable, Sendable {
    public var model: String
    public var effort: String?
    public var provider: String?

    public init(model: String, effort: String?, provider: String? = nil) {
        self.model = model
        self.effort = effort
        self.provider = provider
    }

    public func encode(to encoder: Encoder) throws {
        enum Key: String, CodingKey {
            case model
            case effort
            case provider
        }
        var container = encoder.container(keyedBy: Key.self)
        try container.encode(model, forKey: .model)
        try container.encodeIfPresent(provider, forKey: .provider)
        if let effort {
            try container.encode(effort, forKey: .effort)
        } else {
            try container.encodeNil(forKey: .effort)
        }
    }
}

public enum PhoneOverlay: String, Equatable, Sendable, Identifiable {
    case palette
    case model
    case effort
    case profile

    public var id: String { rawValue }
}

public let everythingProfile = "Everything"

public func profileChoices(_ names: [String]) -> [String] {
    var rows: [String] = []
    for name in names {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed == everythingProfile || rows.contains(trimmed) {
            continue
        }
        rows.append(trimmed)
    }
    return rows + [everythingProfile]
}

public func createProfileField(_ choice: String?) -> String? {
    guard let choice else {
        return nil
    }
    let trimmed = choice.trimmingCharacters(in: .whitespacesAndNewlines)
    if trimmed.isEmpty || trimmed == everythingProfile {
        return nil
    }
    return trimmed
}

public func liveProfileField(_ choice: String) -> String {
    createProfileField(choice) ?? ""
}

public enum PaletteResult: Equatable, Sendable {
    case showNewSession
    case showHelp
    case finished
}

struct SlashParts {
    var name: String
    var argument: String?
}

func slashParts(_ text: String) -> SlashParts? {
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    guard trimmed.hasPrefix("/") else {
        return nil
    }
    let rest = String(trimmed.dropFirst())
    guard let space = rest.firstIndex(where: \.isWhitespace) else {
        return SlashParts(name: rest, argument: nil)
    }
    let name = String(rest[..<space])
    let argument = rest[rest.index(after: space)...].trimmingCharacters(in: .whitespacesAndNewlines)
    if argument.isEmpty {
        return SlashParts(name: name, argument: nil)
    }
    return SlashParts(name: name, argument: argument)
}

public func slashCommand(_ text: String, efforts: [String]) -> SlashCommand {
    guard let parts = slashParts(text), !parts.name.isEmpty else {
        return .ask
    }
    switch (parts.name, parts.argument) {
    case ("model", nil):
        return .openModel
    case ("model", let id?):
        return .setModel(id)
    case ("effort", nil):
        return .openEffort
    case ("effort", let level?) where !level.contains(where: \.isWhitespace) && efforts.contains(level):
        return .setEffort(level)
    case ("compact", nil):
        return .compact
    case ("yolo", nil):
        return .yolo(.toggle)
    case ("yolo", "on"):
        return .yolo(.on)
    case ("yolo", "off"):
        return .yolo(.off)
    default:
        return .ask
    }
}

public func commandCatalog(skills: [Skill]) -> [CommandEntry] {
    var rows = [
        CommandEntry(id: "new-session", title: "New session", hint: "ask where this session should work", kind: .newSession),
        CommandEntry(id: "open-model", title: "Open model", hint: "open the model list", kind: .openModel),
        CommandEntry(id: "effort", title: "Effort", hint: "open the effort list", kind: .openEffort),
        CommandEntry(id: "compact", title: "Compact", hint: "summarize the older transcript", kind: .compact),
        CommandEntry(id: "yolo", title: "Yolo", hint: "allow writes and commands", kind: .yolo),
        CommandEntry(id: "profile", title: "Profile", hint: "limit this session's tools and skills", kind: .profile),
        CommandEntry(id: "cancel", title: "Cancel", hint: "stop the turn", kind: .cancel),
        CommandEntry(id: "archive-session", title: "Archive session", hint: "hide the selected session until it is restored", kind: .archiveSession),
        CommandEntry(id: "unarchive-session", title: "Unarchive session", hint: "restore an archived session", kind: .unarchiveSession),
        CommandEntry(id: "close-session", title: "Delete session", hint: "delete the selected session", kind: .closeSession),
        CommandEntry(id: "help", title: "Help", hint: "list every command", kind: .help),
        CommandEntry(id: "slash-model", title: "/model", hint: "open the model list", kind: .openModel),
        CommandEntry(id: "slash-effort", title: "/effort", hint: "set the reasoning effort", kind: .openEffort),
        CommandEntry(id: "slash-compact", title: "/compact", hint: "summarize the older transcript", kind: .compact),
        CommandEntry(id: "slash-yolo", title: "/yolo", hint: "turn yolo on or off", kind: .yolo),
    ]
    for skill in skills where skill.user_invocable {
        let hint = skill.description.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        rows.append(CommandEntry(id: "skill-" + skill.name, title: "/" + skill.name, hint: hint, kind: .skill(skill.name)))
    }
    return rows
}

public func helpCatalog(skills: [Skill]) -> [CommandEntry] {
    visibleCommands(skills)
}

public func filteredCatalog(skills: [Skill], query: String) -> [CommandEntry] {
    visibleCommands(skills).filter { commandMatches($0, query: query) }
}

private func visibleCommands(_ skills: [Skill]) -> [CommandEntry] {
    commandCatalog(skills: skills).filter { !$0.title.hasPrefix("/") }
}

func commandMatches(_ entry: CommandEntry, query: String) -> Bool {
    if query.isEmpty {
        return true
    }
    let needle = slashless(query)
    if needle.isEmpty {
        return false
    }
    return slashless(entry.title).hasPrefix(needle)
}

func slashless(_ text: String) -> String {
    var value = text.trimmingCharacters(in: .whitespacesAndNewlines)
    while value.hasPrefix("/") {
        value.removeFirst()
    }
    return value.lowercased()
}

public func skillMatches(_ skills: [Skill], draft: String) -> [Skill] {
    let trimmed = draft.drop(while: \.isWhitespace)
    guard trimmed.hasPrefix("/"), !trimmed.contains(where: \.isWhitespace) else {
        return []
    }
    let rest = trimmed.dropFirst()
    let token = rest.split(whereSeparator: \.isWhitespace).first.map(String.init) ?? ""
    let invocable = skills.filter(\.user_invocable)
    let needle = token.lowercased()
    var found = invocable.filter { $0.name.lowercased().hasPrefix(needle) }
    if found.isEmpty {
        found = invocable.filter { $0.name.lowercased().contains(needle) }
    }
    return Array(found.sorted { $0.name < $1.name }.prefix(8))
}

public func filledSkill(_ name: String) -> String {
    "/" + name + " "
}

public func effortChoices(for modelID: String, in models: [Model]) -> [String] {
    models.first { $0.id == modelID }?.reasoning_efforts ?? []
}

public func postedModel(models: [Model], modelID: String, effort: String?) -> ModelPayload {
    let allowed = effortChoices(for: modelID, in: models).contains(effort ?? "") ? effort : nil
    return ModelPayload(model: modelID, effort: allowed)
}

public func modelPostBody(model: String, effort: String?) throws -> Data {
    try JSONEncoder().encode(ModelPayload(model: model, effort: effort))
}
