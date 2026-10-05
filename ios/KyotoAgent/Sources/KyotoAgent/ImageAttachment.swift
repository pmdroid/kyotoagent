import Foundation

nonisolated public struct ImageAttachment: Codable, Equatable, Sendable, Identifiable {
    public var name: String
    public var mimeType: String
    public var data: String

    public var id: String { name + String(data.hashValue) }
    public var bytes: Data? { Data(base64Encoded: data) }

    public init(name: String, mimeType: String, data: String) {
        self.name = name
        self.mimeType = mimeType
        self.data = data
    }
}
