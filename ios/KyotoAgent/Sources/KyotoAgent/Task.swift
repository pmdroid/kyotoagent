import Foundation

nonisolated public enum TaskState: String, Codable, Equatable, Sendable {
    case running
    case exited
    case timed_out
    case stopped
}

nonisolated public struct Task: Codable, Equatable, Sendable {
    public var id: String
    public var argv: [String]
    public var state: TaskState
    public var exit: Int?
    public var tail: String?
}
