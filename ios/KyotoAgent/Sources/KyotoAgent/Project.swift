import Foundation

nonisolated public struct Project: Codable, Equatable, Sendable {
    public var id: String
    public var name: String
    public var path: String
    public var yolo: Bool?
}
