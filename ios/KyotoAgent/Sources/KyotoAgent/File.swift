import Foundation

nonisolated public struct File: Codable, Equatable, Sendable {
    public var path: String
    public var text: String
    public var truncated: Bool
}
