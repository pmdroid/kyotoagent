import SwiftUI

enum Ink {
    static let canvas = Color(uiColor: .systemBackground)
    static let card = Color(uiColor: .secondarySystemBackground)
    static let selected = Color(uiColor: .tertiarySystemFill)
    static let line = Color(uiColor: .separator)
    static let text = Color.primary
    static let faint = Color.secondary
    static let accent = Color.blue
    static let ask = Color.blue
    static let result = Color.green
    static let permission = Color.orange
    static let proof = Color.purple
    static let question = Color.orange
    static let answer = Color.secondary
    static let good = Color.green
    static let bad = Color.red
}

func statusColor(_ status: Status) -> Color {
    switch status {
    case .working:
        return Ink.accent
    case .waiting:
        return Ink.permission
    case .idle:
        return Ink.faint
    }
}

func kindColor(_ kind: CardKind) -> Color {
    switch kind {
    case .ask:
        return Ink.ask
    case .question:
        return Ink.question
    case .answer:
        return Ink.answer
    case .permission:
        return Ink.permission
    case .result:
        return Ink.result
    case .proof, .artifact:
        return Ink.proof
    case .enhance, .btw:
        return Ink.accent
    }
}
