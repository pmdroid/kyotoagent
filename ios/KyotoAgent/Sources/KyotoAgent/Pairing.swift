import Foundation

nonisolated public struct PairingConnection: Equatable, Sendable {
    public let baseURL: URL
    public let token: String?

    public init?(_ text: String) {
        guard var parts = URLComponents(string: text), let scheme = parts.scheme?.lowercased(),
            parts.host != nil, parts.user == nil, parts.password == nil, parts.fragment == nil
        else {
            return nil
        }
        if scheme == "kyotoagent" {
            guard let port = parts.port, (1...65535).contains(port),
                parts.path.isEmpty || parts.path == "/",
                let queries = parts.queryItems, queries.count == 1,
                queries[0].name == "token", let value = queries[0].value, !value.isEmpty,
                !value.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
            else {
                return nil
            }
            parts.scheme = "https"
            parts.query = nil
            parts.path = ""
            guard let url = parts.url else { return nil }
            baseURL = url
            token = value
        } else {
            guard scheme == "https" || scheme == "http", parts.query == nil,
                let url = parts.url
            else {
                return nil
            }
            baseURL = url
            token = nil
        }
    }
}
