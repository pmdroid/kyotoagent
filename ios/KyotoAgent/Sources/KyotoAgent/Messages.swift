import Foundation

nonisolated public struct TurnAccepted: Codable, Equatable, Sendable {
    public var turnId: String
}

nonisolated public struct AskQueued: Codable, Equatable, Sendable {
    public var queued: Bool

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let queued = try container.decode(Bool.self, forKey: .queued)
        guard queued else {
            throw DecodingError.dataCorruptedError(
                forKey: .queued,
                in: container,
                debugDescription: "queued"
            )
        }
        self.queued = queued
    }

    nonisolated public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(queued, forKey: .queued)
    }

    private enum CodingKeys: String, CodingKey {
        case queued
    }
}
