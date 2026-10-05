import SwiftUI

@main
struct KyotoAgentApp: App {
    @State private var connections = ServerConnections(
        store: KeychainBaseURL(account: "servers"),
        legacy: KeychainBaseURL(),
        makeModel: makePhoneModel
    )

    var body: some Scene {
        WindowGroup {
            RootView(model: connections.model)
                .id(ObjectIdentifier(connections.model))
                .environment(connections)
                .tint(Ink.accent)
                .task {
                    if let icon = UIApplication.shared.alternateIconName, ["SimpleIcon", "GoldenIcon"].contains(icon) {
                        try? await UIApplication.shared.setAlternateIconName(nil)
                    }
                    await connections.connectOnLaunch()
                }
                .sheet(isPresented: $connections.showingServers) {
                    ServerSwitcher().environment(connections)
                }
                .onOpenURL { url in
                    guard url.scheme == "kyotoagent", PairingConnection(url.absoluteString) != nil else { return }
                    connections.showingServers = true
                    Swift.Task {
                        await connections.connectOnLaunch()
                        await connections.connect(url.absoluteString)
                    }
                }
        }
    }
}

@MainActor
func makePhoneModel(_ server: SavedServer) -> AppModel {
    let root = server.id == "legacy"
        ? phoneSupportRoot()
        : phoneSupportRoot().appendingPathComponent("servers").appendingPathComponent(server.id)
    let defaults = server.id == "legacy"
        ? UserDefaults.standard
        : UserDefaults(suiteName: "sh.pascal.kyotoagent.server." + server.id)!
    let views = ViewCache(directory: root.appendingPathComponent("views", isDirectory: true))
    views.excludeFromBackup()
    return AppModel(
        store: MemoryBaseURL(value: server.connection),
        drafts: DraftFiles(directory: root.appendingPathComponent("drafts", isDirectory: true)),
        views: views,
        preferences: LastSessionPreference(defaults: defaults),
        transport: { URLSessionTransport.system() }
    )
}

@MainActor
func phoneSupportRoot() -> URL {
    let fileManager = FileManager.default
    let base = (try? fileManager.url(
        for: .applicationSupportDirectory,
        in: .userDomainMask,
        appropriateFor: nil,
        create: true
    )) ?? fileManager.temporaryDirectory
    let root = base.appendingPathComponent("KyotoAgent", isDirectory: true)
    try? fileManager.createDirectory(at: root, withIntermediateDirectories: true)
    return root
}
