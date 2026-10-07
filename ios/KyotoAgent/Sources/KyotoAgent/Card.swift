import Foundation

nonisolated public enum CardKind: String, Codable, CaseIterable, Equatable, Sendable {
    case ask
    case question
    case answer
    case permission
    case result
    case proof
    case artifact
    case enhance
}

nonisolated public enum CardEdge: Equatable, Sendable {
    case leading
    case trailing
}

nonisolated public func cardEdge(_ kind: CardKind) -> CardEdge {
    switch kind {
    case .ask, .answer:
        return .trailing
    case .question, .permission, .result, .proof, .artifact, .enhance:
        return .leading
    }
}

nonisolated public func cardHoldsLink(_ body: CardBody) -> Bool {
    switch body {
    case .ask(let text), .answer(let text):
        return textHoldsLink(text.text)
    case .question(let question):
        return textHoldsLink(question.text)
    case .result(let result):
        if textHoldsLink(result.text) {
            return true
        }
        if let note = result.note, textHoldsLink(note) {
            return true
        }
        return false
    case .proof(let proof):
        return textHoldsLink(proof.text)
    case .enhance(let enhance):
        return textHoldsLink(enhance.text)
    case .permission, .artifact:
        return false
    }
}

nonisolated func textHoldsLink(_ text: String) -> Bool {
    text.contains("](") || text.contains("http://") || text.contains("https://")
}

nonisolated public enum Decision: String, Codable, Equatable, Sendable {
    case allow_once
    case allow_session
    case deny
}

nonisolated public struct TextCard: Codable, Equatable, Sendable {
    public var text: String
    public var images: [ImageAttachment]? = nil
}

nonisolated public struct QuestionCard: Codable, Equatable, Sendable {
    public var answer: String?
    public var choices: [String]
    public var eventId: String
    public var text: String
    public var visuals: [QuestionVisual]? = nil
}

nonisolated public struct QuestionVisual: Codable, Equatable, Sendable {
    public var title: String
    public var alt: String
    public var image: ImageAttachment
    public var source: String?
}

nonisolated public struct PermissionCard: Codable, Equatable, Sendable {
    public var action: String
    public var argv: [String]?
    public var decision: Decision?
    public var diff: [String]?
    public var eventId: String
    public var path: String?
    public var timeoutSec: Int?
}

nonisolated public struct ResultCard: Codable, Equatable, Sendable {
    public var text: String
    public var note: String?
}

nonisolated public struct ProofItem: Codable, Equatable, Sendable {
    public var id: String
    public var kind: String
    public var outcome: String
    public var argv: [String]?
    public var exit: Int?
    public var tail: String?
}

nonisolated public struct ProofCard: Codable, Equatable, Sendable {
    public var text: String
    public var items: [ProofItem]

    public init(text: String, items: [ProofItem]) {
        self.text = text
        self.items = items
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        text = try container.decode(String.self, forKey: .text)
        items = try container.decodeIfPresent([ProofItem].self, forKey: .items) ?? []
    }
}

nonisolated public struct EnhanceCard: Codable, Equatable, Sendable {
    public var text: String
    public var source: String
    public var model: String
    public var eventId: String
    public var error: String?
}

nonisolated public enum CardBody: Equatable, Sendable {
    case ask(TextCard)
    case question(QuestionCard)
    case answer(TextCard)
    case permission(PermissionCard)
    case result(ResultCard)
    case proof(ProofCard)
    case artifact(ArtifactCard)
    case enhance(EnhanceCard)
}

nonisolated public struct Card: Codable, Equatable, Sendable {
    public var id: String
    public var kind: CardKind
    public var at: String
    public var body: CardBody

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        kind = try container.decode(CardKind.self, forKey: .kind)
        at = try container.decode(String.self, forKey: .at)
        switch kind {
        case .ask:
            body = .ask(try container.decode(TextCard.self, forKey: .body))
        case .question:
            body = .question(try container.decode(QuestionCard.self, forKey: .body))
        case .answer:
            body = .answer(try container.decode(TextCard.self, forKey: .body))
        case .permission:
            body = .permission(try container.decode(PermissionCard.self, forKey: .body))
        case .result:
            body = .result(try container.decode(ResultCard.self, forKey: .body))
        case .artifact:
            body = .artifact(try container.decode(ArtifactCard.self, forKey: .body))
        case .proof:
            body = .proof(try container.decode(ProofCard.self, forKey: .body))
        case .enhance:
            body = .enhance(try container.decode(EnhanceCard.self, forKey: .body))
        }
    }

    nonisolated public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(kind, forKey: .kind)
        try container.encode(at, forKey: .at)
        switch body {
        case .ask(let text):
            try container.encode(text, forKey: .body)
        case .question(let question):
            try container.encode(question, forKey: .body)
        case .answer(let text):
            try container.encode(text, forKey: .body)
        case .permission(let permission):
            try container.encode(permission, forKey: .body)
        case .result(let result):
            try container.encode(result, forKey: .body)
        case .artifact(let artifact):
            try container.encode(artifact, forKey: .body)
        case .proof(let proof):
            try container.encode(proof, forKey: .body)
        case .enhance(let enhance):
            try container.encode(enhance, forKey: .body)
        }
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case kind
        case at
        case body
    }
}

nonisolated func cardHasTextPreview(_ body: CardBody) -> Bool {
    let text: String
    switch body {
    case .result(let result):
        text = result.text
    case .question(let question):
        text = question.text
    case .proof(let proof):
        text = proof.text
    default:
        return false
    }
    let head = String(text.prefix(201))
    return head.count > 200 || head.components(separatedBy: "\n").count > 3
}

nonisolated func cardTextPreview(_ body: CardBody) -> CardBody {
    guard cardHasTextPreview(body) else { return body }
    switch body {
    case .result(var result):
        result.text = clippedCardText(result.text)
        return .result(result)
    case .question(var question):
        question.text = clippedCardText(question.text)
        return .question(question)
    case .proof(var proof):
        proof.text = clippedCardText(proof.text)
        return .proof(proof)
    default:
        return body
    }
}

nonisolated private func clippedCardText(_ text: String) -> String {
    String(text.prefix(200)).components(separatedBy: "\n").prefix(3).joined(separator: "\n")
}
