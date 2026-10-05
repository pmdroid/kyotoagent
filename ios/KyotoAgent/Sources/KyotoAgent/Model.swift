import Foundation

nonisolated public struct Model: Codable, Hashable, Sendable {
    public var id: String
    public var aliases: [String]
    public var reasoning_efforts: [String]
    public var context_length: Int?
    public var provider: String?

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        aliases = try container.decodeIfPresent([String].self, forKey: .aliases) ?? []
        reasoning_efforts = try container.decodeIfPresent([String].self, forKey: .reasoning_efforts) ?? []
        context_length = try container.decodeIfPresent(Int.self, forKey: .context_length)
        provider = try container.decodeIfPresent(String.self, forKey: .provider)
    }

    nonisolated public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        if !aliases.isEmpty {
            try container.encode(aliases, forKey: .aliases)
        }
        if !reasoning_efforts.isEmpty {
            try container.encode(reasoning_efforts, forKey: .reasoning_efforts)
        }
        try container.encodeIfPresent(context_length, forKey: .context_length)
        try container.encodeIfPresent(provider, forKey: .provider)
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case aliases
        case reasoning_efforts
        case context_length
        case provider
    }
}
