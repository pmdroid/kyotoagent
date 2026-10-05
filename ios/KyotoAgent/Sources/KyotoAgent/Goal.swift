import Foundation

nonisolated public enum GoalStatus: String, Codable, Equatable, Sendable {
    case active
    case paused
    case complete
    case budget_exhausted

    public var title: String {
        switch self {
        case .active: "Active"
        case .paused: "Paused"
        case .complete: "Verified"
        case .budget_exhausted: "Budget exhausted"
        }
    }
}

nonisolated public struct Goal: Codable, Equatable, Sendable {
    public var objective: String
    public var status: GoalStatus
    public var token_budget: UInt64?
    public var tokens_used: UInt64
    public var rounds: UInt64
    public var verification: String
}
