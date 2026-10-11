import Foundation

nonisolated public enum AnswerSheet: Equatable, Sendable, Identifiable {
    case permission(PermissionCard)
    case question(QuestionCard)

    public var eventId: String {
        switch self {
        case .permission(let card):
            card.eventId
        case .question(let card):
            card.eventId
        }
    }

    public var id: String { eventId }

    public var isPermission: Bool {
        if case .permission = self { return true }
        return false
    }

    public var isCloseoutAcceptance: Bool {
        guard case .question(let question) = self else { return false }
        return question.choices == ["Stop", "Accept failed closeout for this session"]
    }

    public static func front(
        cards: [Card],
        dismissed: String?,
        dismissedPermission: String? = nil
    ) -> AnswerSheet? {
        var permission: PermissionCard?
        var question: QuestionCard?
        for card in cards.reversed() {
            switch card.body {
            case .permission(let body)
                where body.decision == nil && permission == nil && dismissedPermission != body.eventId:
                permission = body
            case .question(let body) where body.answer == nil && question == nil && dismissed != body.eventId:
                question = body
            default:
                break
            }
        }
        if let permission {
            return .permission(permission)
        }
        if let question {
            return .question(question)
        }
        return nil
    }

    public static func questionDismissed(byNil sheet: AnswerSheet?) -> String? {
        guard case .question(let question) = sheet else {
            return nil
        }
        return question.eventId
    }

    public static func permissionDismissed(byNil sheet: AnswerSheet?) -> String? {
        guard case .permission(let permission) = sheet else {
            return nil
        }
        return permission.eventId
    }
}

public enum HardwareKey: Equatable, Sendable {
    case escape
    case controlX
}

public enum HardwareKeyAction: Equatable, Sendable {
    case dismissPopup
    case cancelTurn
    case discardEnhance
}

public func hardwareKeyAction(key: HardwareKey, popupOpen: Bool, enhanceOpen: Bool) -> HardwareKeyAction? {
    switch key {
    case .controlX:
        return .cancelTurn
    case .escape:
        if popupOpen {
            return .dismissPopup
        }
        if enhanceOpen {
            return .discardEnhance
        }
        return nil
    }
}

public enum EnhancePost: Equatable, Sendable {
    case use
    case edit(String)
    case discard
}

public func enhanceAnswer(_ post: EnhancePost) -> (choice: String, text: String?) {
    switch post {
    case .use:
        return ("use", nil)
    case .edit(let text):
        return ("revise", text)
    case .discard:
        return ("discard", nil)
    }
}

public func openEnhanceCard(_ cards: [Card]) -> EnhanceCard? {
    for card in cards.reversed() {
        if case .enhance(let body) = card.body {
            return body
        }
    }
    return nil
}

public enum TranscriptCover: Equatable, Sendable {
    case answer(AnswerSheet)
    case dock
    case thinking
    case context
    case command

    public static func select(
        answer: AnswerSheet?,
        dock: Bool,
        thinking: Bool,
        context: Bool,
        command: Bool
    ) -> TranscriptCover? {
        if let answer {
            return .answer(answer)
        }
        if dock {
            return .dock
        }
        if thinking {
            return .thinking
        }
        if context {
            return .context
        }
        if command {
            return .command
        }
        return nil
    }
}

nonisolated public struct AnswerPayload: Codable, Equatable, Sendable {
    public var id: String
    public var choice: String
    public var text: String?

    public init(id: String, choice: String, text: String? = nil) {
        self.id = id
        self.choice = choice
        self.text = text
    }

    public init(sheet: AnswerSheet, choice: String) {
        self.init(id: sheet.eventId, choice: choice)
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        choice = try container.decode(String.self, forKey: .choice)
        text = try container.decodeIfPresent(String.self, forKey: .text)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(choice, forKey: .choice)
        if let text, !text.isEmpty {
            try container.encode(text, forKey: .text)
        }
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case choice
        case text
    }
}
