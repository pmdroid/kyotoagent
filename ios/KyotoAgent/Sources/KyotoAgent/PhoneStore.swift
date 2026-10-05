import Foundation

nonisolated public enum RefreshCadence {
    public static let sessions: Duration = .seconds(2)
    public static let view: Duration = .seconds(1)
}

nonisolated public struct ComposerDraft: Equatable, Sendable {
    public var text: String
    public var notice: String?

    public init(text: String, notice: String? = nil) {
        self.text = text
        self.notice = notice
    }

    public mutating func sent(_ result: Result<AskAcceptance, HostError>) {
        switch result {
        case .success:
            text = ""
            notice = nil
        case .failure(let error):
            notice = error.serverMessage
        }
    }
}

nonisolated public struct TranscriptWindow: Equatable, Sendable {
    public private(set) var view: View?

    public init() {}

    public mutating func receive(_ next: View) -> Bool {
        var refreshed = next
        if let view, view.revision == next.revision {
            refreshed.cards = view.cards
        }
        guard view != refreshed else { return false }
        view = refreshed
        return true
    }
}

nonisolated public protocol BaseURLStoring: Sendable {
    nonisolated func load() throws -> String?
    nonisolated func save(_ url: String) throws
}

nonisolated public final class MemoryBaseURL: BaseURLStoring, @unchecked Sendable {
    public var value: String?
    public private(set) var saves: [String] = []

    public init(value: String? = nil) {
        self.value = value
    }

    nonisolated public func load() throws -> String? {
        value
    }

    nonisolated public func save(_ url: String) throws {
        saves.append(url)
        value = url
    }
}

#if canImport(Security)
import Security

nonisolated public struct KeychainBaseURL: BaseURLStoring {
    public var service: String
    public var account: String

    public init(service: String = "sh.pascal.kyotoagent", account: String = "baseURL") {
        self.service = service
        self.account = account
    }

    nonisolated public func load() throws -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound {
            return nil
        }
        guard status == errSecSuccess, let data = item as? Data, let text = String(data: data, encoding: .utf8) else {
            throw KeychainFailure(status: status)
        }
        return text
    }

    nonisolated public func save(_ url: String) throws {
        let data = Data(url.utf8)
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        let update = [kSecValueData as String: data]
        let status = SecItemUpdate(query as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var add = query
            add[kSecValueData as String] = data
            let added = SecItemAdd(add as CFDictionary, nil)
            guard added == errSecSuccess else {
                throw KeychainFailure(status: added)
            }
            return
        }
        guard status == errSecSuccess else {
            throw KeychainFailure(status: status)
        }
    }
}

nonisolated public struct KeychainFailure: Error, Equatable {
    public var status: OSStatus
}
#endif

nonisolated public struct DraftFiles: Sendable {
    public var directory: URL

    public init(directory: URL) {
        self.directory = directory
    }

    nonisolated public func load(_ id: String) -> String {
        guard let url = file(id), let data = try? Data(contentsOf: url) else {
            return ""
        }
        return String(decoding: data, as: UTF8.self)
    }

    nonisolated public func save(_ id: String, _ text: String) throws {
        guard let url = file(id) else {
            return
        }
        if text.isEmpty {
            if FileManager.default.fileExists(atPath: url.path) {
                try FileManager.default.removeItem(at: url)
            }
            return
        }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data(text.utf8).write(to: url, options: .atomic)
    }

    private func file(_ id: String) -> URL? {
        guard let name = sessionFileName(id) else {
            return nil
        }
        return directory.appendingPathComponent(name, isDirectory: false)
    }
}

nonisolated public struct ViewCache: Sendable {
    public var directory: URL

    public init(directory: URL) {
        self.directory = directory
    }

    nonisolated public func load(_ id: String) -> View? {
        guard let url = file(id), let data = try? Data(contentsOf: url) else {
            return nil
        }
        return try? JSONDecoder().decode(View.self, from: data)
    }

    nonisolated public func store(_ id: String, _ view: View) throws {
        guard let url = file(id) else {
            return
        }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let data = try JSONEncoder().encode(view)
        try data.write(to: url, options: .atomic)
        exclude(url)
    }

    nonisolated public func excludeFromBackup() {
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        exclude(directory)
    }

    private func file(_ id: String) -> URL? {
        guard let name = sessionFileName(id) else {
            return nil
        }
        return directory.appendingPathComponent(name + ".json", isDirectory: false)
    }

    private func exclude(_ url: URL) {
#if os(iOS) || os(macOS)
        var copy = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? copy.setResourceValues(values)
#endif
    }
}

nonisolated public struct LastSessionPreference {
    public var defaults: UserDefaults
    public var key: String

    public init(defaults: UserDefaults, key: String = "lastSessionId") {
        self.defaults = defaults
        self.key = key
    }

    public var id: String? {
        get { defaults.string(forKey: key) }
        nonmutating set {
            if let newValue {
                defaults.set(newValue, forKey: key)
            } else {
                defaults.removeObject(forKey: key)
            }
        }
    }

    public func dismissedQuestion(session: String) -> String? {
        defaults.string(forKey: dismissedQuestionKey(session))
    }

    public func dismissQuestion(_ eventId: String, session: String) {
        defaults.set(eventId, forKey: dismissedQuestionKey(session))
    }

    public func dismissedPermission(session: String) -> String? {
        defaults.string(forKey: dismissedPermissionKey(session))
    }

    public func dismissPermission(_ eventId: String, session: String) {
        defaults.set(eventId, forKey: dismissedPermissionKey(session))
    }

    public func clearDismissedPermission(session: String) {
        defaults.removeObject(forKey: dismissedPermissionKey(session))
    }

    private func dismissedQuestionKey(_ session: String) -> String {
        "dismissedQuestion." + session
    }

    private func dismissedPermissionKey(_ session: String) -> String {
        "dismissedPermission." + session
    }
}

nonisolated func sessionFileName(_ id: String) -> String? {
    let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_")
    guard !id.isEmpty, id.unicodeScalars.allSatisfy({ allowed.contains($0) }) else {
        return nil
    }
    return id
}
