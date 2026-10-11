import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

nonisolated public struct HostResponse: Equatable, Sendable {
    public var status: Int
    public var body: Data

    public init(status: Int, body: Data) {
        self.status = status
        self.body = body
    }
}

nonisolated public enum HostError: Error, Equatable, Sendable {
    case status(Int, String)
    case transport(String)

    public var serverMessage: String {
        switch self {
        case .transport(let message):
            return message
        case .status(let status, let body):
            if let parsed = errorMessage(body) {
                return parsed
            }
            if body.isEmpty {
                return String(status)
            }
            return body
        }
    }

    public var statusAndBody: String {
        switch self {
        case .transport(let message):
            return message
        case .status(let status, let body):
            if body.isEmpty {
                return String(status)
            }
            return String(status) + "\n" + body
        }
    }
}

nonisolated public protocol HostTransport: Sendable {
    nonisolated func send(_ request: URLRequest) async throws -> HostResponse
}

nonisolated public enum ServeCall: Equatable, Sendable {
    case pair(PairingClient)
    case registerDevice(DeviceRegistration)
    case deleteDevice(String)
    case sessions
    case projects
    case view(String)
    case message(id: String, text: String, images: [ImageAttachment] = [])
    case btw(id: String, text: String)
    case cancel(String)
    case create(workspace: String, worktree: Bool, profile: String?)
    case delete(String, deleteWorkspace: Bool = false)
    case archive(String, archived: Bool)
    case answer(session: String, eventId: String, choice: String, text: String?)
    case file(id: String, path: String)
    case artifacts(String)
    case artifact(id: String, fileId: String)
    case task(id: String, taskId: String)
    case models
    case compact(String)
    case yolo(id: String, yolo: Bool)
    case setModel(id: String, model: String, effort: String?, provider: String? = nil, saveAsDefault: Bool = false)
    case profiles
    case setProfile(id: String, profile: String)
}

nonisolated public struct CreateSessionBody: Codable, Equatable, Sendable {
    public var workspace: String
    public var worktree: Bool
    public var profile: String?

    public init(workspace: String, worktree: Bool, profile: String? = nil) {
        self.workspace = workspace
        self.worktree = worktree
        self.profile = profile
    }

    public func encode(to encoder: Encoder) throws {
        enum Key: String, CodingKey {
            case workspace
            case worktree
            case profile
        }
        var container = encoder.container(keyedBy: Key.self)
        try container.encode(workspace, forKey: .workspace)
        try container.encode(worktree, forKey: .worktree)
        if let profile {
            try container.encode(profile, forKey: .profile)
        }
    }
}

nonisolated public struct CreatedSession: Codable, Equatable, Sendable {
    public var id: String
    public var workspace: String
    public var status: Status
}

nonisolated public enum AskAcceptance: Equatable, Sendable {
    case started(String)
    case queued
    case ignored
}

nonisolated public struct URLSessionTransport: HostTransport {
    public var session: URLSession

    public init(session: URLSession) {
        self.session = session
    }

    public static func system() -> URLSessionTransport {
        URLSessionTransport(session: URLSession(configuration: systemConfiguration(), delegate: NoRedirects(), delegateQueue: nil))
    }

    public static func systemConfiguration() -> URLSessionConfiguration {
        let configuration = URLSessionConfiguration.default
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.urlCache = nil
        return configuration
    }

    nonisolated public func send(_ request: URLRequest) async throws -> HostResponse {
        do {
            let (data, response) = try await session.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            return HostResponse(status: status, body: data)
        } catch let error as HostError {
            throw error
        } catch {
            throw HostError.transport(error.localizedDescription)
        }
    }
}

nonisolated private final class NoRedirects: NSObject, URLSessionTaskDelegate, Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping @Sendable (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

nonisolated public struct ServeClient: Sendable {
    public var baseURL: URL
    public var transport: any HostTransport
    public var token: String?

    public init(baseURL: URL, transport: any HostTransport, token: String? = nil) {
        self.baseURL = baseURL
        self.transport = transport
        self.token = token
    }

    nonisolated public func registerDevice(_ device: DeviceRegistration) async throws {
        let response = try await send(.registerDevice(device))
        guard (200...299).contains(response.status) else {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func deleteDevice(_ id: String) async throws {
        let response = try await send(.deleteDevice(id))
        guard (200...299).contains(response.status) else {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func sessions() async throws -> [Session] {
        try await decodeList(.sessions)
    }

    nonisolated public func pair(identity: PairingClient = .ios) async throws -> PairingInfo {
        let response = try await send(.pair(identity))
        guard response.status == 200 else {
            throw HostError.status(response.status, responseText(response.body))
        }
        guard let info = try? JSONDecoder().decode(PairingInfo.self, from: response.body),
              info.confirms(identity),
              token?.hasPrefix("pair_") != true || info.access_token != nil else {
            throw HostError.transport("The server did not confirm pairing identity.")
        }
        return info
    }

    nonisolated public func projects() async throws -> [Project] {
        let response = try await send(.projects)
        guard response.status == 200 else {
            throw HostError.status(response.status, responseText(response.body))
        }
        do {
            return try JSONDecoder().decode([Project].self, from: response.body)
        } catch {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func profiles() async throws -> [String] {
        let response = try await send(.profiles)
        guard response.status == 200 else {
            throw HostError.status(response.status, responseText(response.body))
        }
        do {
            return try JSONDecoder().decode([String].self, from: response.body)
        } catch {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func createSession(workspace: String, worktree: Bool, profile: String? = nil) async throws -> CreatedSession {
        let response = try await send(.create(workspace: workspace, worktree: worktree, profile: profile))
        guard response.status == 201 else {
            throw HostError.status(response.status, responseText(response.body))
        }
        do {
            return try JSONDecoder().decode(CreatedSession.self, from: response.body)
        } catch {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func deleteSession(_ id: String, deleteWorkspace: Bool = false) async throws {
        try await expectEmpty(.delete(id, deleteWorkspace: deleteWorkspace))
    }

    nonisolated public func setArchived(_ id: String, archived: Bool) async throws {
        try await expectEmpty(.archive(id, archived: archived))
    }

    private func decodeList(_ call: ServeCall) async throws -> [Session] {
        let response = try await send(call)
        guard response.status == 200 else {
            throw HostError.status(response.status, responseText(response.body))
        }
        do {
            return try JSONDecoder().decode([Session].self, from: response.body)
        } catch {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    private func expectEmpty(_ call: ServeCall) async throws {
        let response = try await send(call)
        guard response.status == 204 else {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func view(_ id: String) async throws -> View {
        let response = try await send(.view(id))
        guard response.status == 200 else {
            throw HostError.status(response.status, responseText(response.body))
        }
        do {
            return try JSONDecoder().decode(View.self, from: response.body)
        } catch {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func ask(_ id: String, text: String, images: [ImageAttachment] = []) async throws -> AskAcceptance {
        let side = slashParts(text)?.name == "btw"
        let response = try await send(side ? .btw(id: id, text: slashParts(text)?.argument ?? "") : .message(id: id, text: text, images: images))
        if side, response.status == 202, let json = try? JSONSerialization.jsonObject(with: response.body) as? [String: String], let id = json["btwId"] { return .started(id) }
        if response.status == 202 {
            if let started = try? JSONDecoder().decode(TurnAccepted.self, from: response.body),
               !started.turnId.isEmpty {
                return .started(started.turnId)
            }
            if (try? JSONDecoder().decode(AskQueued.self, from: response.body)) != nil {
                return .queued
            }
            if let body = try? JSONSerialization.jsonObject(with: response.body) as? [String: Any], body.isEmpty {
                return .ignored
            }
        }
        throw HostError.status(response.status, responseText(response.body))
    }

    nonisolated public func cancel(_ id: String) async throws {
        let response = try await send(.cancel(id))
        guard response.status == 204 else {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func answer(
        _ session: String,
        eventId: String,
        choice: String,
        text: String? = nil
    ) async throws {
        let response = try await send(.answer(session: session, eventId: eventId, choice: choice, text: text))
        guard response.status == 204 else {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func artifacts(_ session: String) async throws -> [ArtifactVersion] {
        let response = try await send(.artifacts(session))
        guard response.status == 200 else { throw HostError.status(response.status, responseText(response.body)) }
        return try JSONDecoder().decode([ArtifactVersion].self, from: response.body)
    }

    nonisolated public func artifact(_ session: String, file: ArtifactFile) async throws -> Data {
        let hex = CharacterSet(charactersIn: "0123456789abcdef")
        guard file.id.count == 32, file.sha256.count == 64,
              file.id.unicodeScalars.allSatisfy({ hex.contains($0) }),
              file.sha256.unicodeScalars.allSatisfy({ hex.contains($0) }),
              !session.isEmpty, !session.contains("/"), !session.contains("\\"),
              !file.name.isEmpty, file.name != ".", file.name != "..",
              !file.name.contains("/"), !file.name.contains("\\"),
              !file.name.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
            throw HostError.transport("Invalid artifact metadata")
        }
        let response = try await send(.artifact(id: session, fileId: file.id))
        guard response.status == 200 else { throw HostError.status(response.status, responseText(response.body)) }
        guard UInt64(response.body.count) == file.size, artifactDigest(response.body) == file.sha256 else {
            throw HostError.transport("Artifact integrity check failed")
        }
        return response.body
    }

    nonisolated public func file(_ id: String, path: String) async throws -> File {
        let response = try await send(.file(id: id, path: path))
        guard response.status == 200 else {
            throw HostError.status(response.status, responseText(response.body))
        }
        do {
            return try JSONDecoder().decode(File.self, from: response.body)
        } catch {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func models() async throws -> [Model] {
        let response = try await send(.models)
        guard response.status == 200 else {
            throw HostError.status(response.status, responseText(response.body))
        }
        do {
            return try JSONDecoder().decode([Model].self, from: response.body)
        } catch {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func compact(_ id: String) async throws {
        let response = try await send(.compact(id))
        guard response.status == 202 || response.status == 204 else {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    nonisolated public func setYolo(_ id: String, yolo: Bool) async throws {
        try await expectEmpty(.yolo(id: id, yolo: yolo))
    }

    nonisolated public func setModel(_ id: String, model: String, effort: String?, provider: String? = nil, saveAsDefault: Bool = false) async throws {
        try await expectEmpty(.setModel(id: id, model: model, effort: effort, provider: provider, saveAsDefault: saveAsDefault))
    }

    nonisolated public func setProfile(_ id: String, profile: String) async throws {
        try await expectEmpty(.setProfile(id: id, profile: profile))
    }

    nonisolated public func task(_ id: String, _ taskId: String) async throws -> Task {
        let response = try await send(.task(id: id, taskId: taskId))
        guard response.status == 200 else {
            throw HostError.status(response.status, responseText(response.body))
        }
        do {
            return try JSONDecoder().decode(Task.self, from: response.body)
        } catch {
            throw HostError.status(response.status, responseText(response.body))
        }
    }

    private func send(_ call: ServeCall) async throws -> HostResponse {
        var request = serveRequest(baseURL: baseURL, call: call)
        if let token {
            request.setValue("Bearer " + token, forHTTPHeaderField: "Authorization")
        }
        return try await transport.send(request)
    }
}

nonisolated public func serveRequest(baseURL: URL, call: ServeCall) -> URLRequest {
    let path: String
    var method = "GET"
    var body: Data?
    switch call {
    case .pair(let identity):
        path = "/v1/pair"
        method = "POST"
        body = try? JSONEncoder().encode(identity)
    case .registerDevice(let device):
        path = "/v1/devices"
        method = "PUT"
        body = try? JSONEncoder().encode(device)
    case .deleteDevice(let id):
        path = "/v1/devices"
        method = "DELETE"
        body = try? JSONEncoder().encode(DeviceDeletion(id: id))
    case .sessions:
        path = "/v1/sessions"
    case .projects:
        path = "/v1/projects"
    case .view(let id):
        path = "/v1/sessions/" + id + "/view"
    case .message(let id, let text, let images):
        path = "/v1/sessions/" + id + "/messages"
        method = "POST"
        body = try? JSONEncoder().encode(MessagePayload(text: text, images: images.isEmpty ? nil : images))
    case .btw(let id, let text):
        path = "/v1/sessions/" + id + "/btw"
        method = "POST"
        body = try? JSONEncoder().encode(MessagePayload(text: text, images: nil))
    case .cancel(let id):
        path = "/v1/sessions/" + id + "/cancel"
        method = "POST"
    case .create(let workspace, let worktree, let profile):
        path = "/v1/sessions"
        method = "POST"
        body = try? JSONEncoder().encode(CreateSessionBody(workspace: workspace, worktree: worktree, profile: profile))
    case .profiles:
        path = "/v1/profiles"
    case .setProfile(let id, let profile):
        path = "/v1/sessions/" + id + "/profile"
        method = "POST"
        body = try? JSONEncoder().encode(ProfilePayload(profile: profile))
    case .delete(let id, let deleteWorkspace):
        path = "/v1/sessions/" + id + (deleteWorkspace ? "?delete_workspace=true" : "")
        method = "DELETE"
    case .archive(let id, let archived):
        path = "/v1/sessions/" + id + "/archive"
        method = "POST"
        body = try? JSONEncoder().encode(ArchivePayload(archived: archived))
    case .answer(let session, let eventId, let choice, let text):
        path = "/v1/sessions/" + session + "/answers"
        method = "POST"
        body = try? JSONEncoder().encode(AnswerPayload(id: eventId, choice: choice, text: text))
    case .artifacts(let id):
        path = "/v1/sessions/" + id + "/artifacts"
    case .artifact(let id, let fileId):
        path = "/v1/sessions/" + id + "/artifacts/files/" + fileId
    case .file(let id, let filePath):
        path = "/v1/sessions/" + id + "/file?path=" + encodeQueryPath(filePath)
    case .task(let id, let taskId):
        path = "/v1/sessions/" + id + "/tasks/" + taskId
    case .models:
        path = "/v1/models"
    case .compact(let id):
        path = "/v1/sessions/" + id + "/compact"
        method = "POST"
    case .yolo(let id, let yolo):
        path = "/v1/sessions/" + id + "/yolo"
        method = "POST"
        body = try? JSONEncoder().encode(YoloPayload(yolo: yolo))
    case .setModel(let id, let model, let effort, let provider, let saveAsDefault):
        path = "/v1/sessions/" + id + (saveAsDefault ? "/model" : "/model/session")
        method = "POST"
        body = try? JSONEncoder().encode(ModelPayload(model: model, effort: effort, provider: provider))
    }
    var request = URLRequest(url: serveURL(base: baseURL, path: path))
    request.httpMethod = method
    request.httpBody = body
    if body != nil {
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    }
    return request
}

nonisolated public func serveURL(base: URL, path: String) -> URL {
    var text = base.absoluteString
    while text.hasSuffix("/") {
        text.removeLast()
    }
    return URL(string: text + path) ?? base
}

nonisolated struct MessagePayload: Encodable {
    var text: String
    var images: [ImageAttachment]? = nil
}

nonisolated struct YoloPayload: Encodable {
    var yolo: Bool
}

nonisolated struct ArchivePayload: Encodable {
    var archived: Bool
}

nonisolated struct ProfilePayload: Encodable {
    var profile: String
}

nonisolated func encodeQueryPath(_ path: String) -> String {
    var encoded = ""
    for byte in path.utf8 {
        if isQueryPathByte(byte) {
            encoded.append(Character(UnicodeScalar(byte)))
        } else {
            encoded.append(String(format: "%%%02X", byte))
        }
    }
    return encoded
}

nonisolated func isQueryPathByte(_ byte: UInt8) -> Bool {
    switch byte {
    case 65...90, 97...122, 48...57, 45, 95, 46, 126, 47:
        return true
    default:
        return false
    }
}

nonisolated func responseText(_ data: Data) -> String {
    String(decoding: data, as: UTF8.self)
}

nonisolated func errorMessage(_ body: String) -> String? {
    nonisolated struct ErrorBody: Decodable {
        var error: String
    }
    guard let data = body.data(using: .utf8),
          let parsed = try? JSONDecoder().decode(ErrorBody.self, from: data),
          !parsed.error.isEmpty
    else {
        return nil
    }
    return parsed.error
}
