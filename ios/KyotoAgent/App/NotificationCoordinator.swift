import SwiftUI
import UserNotifications
import UIKit

@MainActor
@Observable
final class NotificationCoordinator {
    static let shared = NotificationCoordinator()
    var enabled: Bool
    var permission = NotificationPermission.notDetermined
    var status: String?
    var showingSettings = false
    private(set) var connections: ServerConnections?
    private var token: Data?
    private var uploaded: [String: DeviceRegistration] = [:]
    private var cleared: Set<String> = []
    private var syncedConnections: [String: String] = [:]
    private var syncing = false
    private var syncAgain = false
    private var pendingRoute: NotificationRoute?
    private let installationID: String
    private let defaults = UserDefaults.standard
    private var removals: [SavedServer] = []
    private let removalStore = KeychainBaseURL(account: "notificationRemovals")

    private init() {
        enabled = UserDefaults.standard.bool(forKey: "notificationsEnabled")
        let id = UserDefaults.standard.string(forKey: "notificationInstallationID") ?? UUID().uuidString
        installationID = id
        UserDefaults.standard.set(id, forKey: "notificationInstallationID")
        if let text = try? removalStore.load(),
           let saved = try? JSONDecoder().decode([SavedServer].self, from: Data(text.utf8)) { removals = saved }
    }

    func attach(_ connections: ServerConnections) async {
        self.connections = connections
        connections.notificationsChanged = { [weak self] in await self?.sync() }
        connections.serverRemoved = { [weak self] server in
            self?.removals.append(server)
            self?.saveRemovals()
            Swift.Task { await self?.sync() }
        }
        await refresh()
        if let route = pendingRoute {
            pendingRoute = nil
            await open(route)
        }
    }

    func refresh() async {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral: permission = .authorized
        case .denied: permission = .denied
        default: permission = .notDetermined
        }
        if permission.shouldRegister(enabled: enabled) {
            UIApplication.shared.registerForRemoteNotifications()
        } else {
            UIApplication.shared.unregisterForRemoteNotifications()
        }
        await sync()
    }

    func enable() async {
        do {
            let granted = try await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
            enabled = granted
            cleared = []
            defaults.set(granted, forKey: "notificationsEnabled")
            status = granted ? nil : "Notifications are blocked. Allow them in iOS Settings."
            await refresh()
        } catch {
            status = error.localizedDescription
        }
    }

    func disable() async {
        enabled = false
        cleared = []
        defaults.set(false, forKey: "notificationsEnabled")
        UIApplication.shared.unregisterForRemoteNotifications()
        await sync()
    }

    func registered(_ data: Data) async {
        token = data
        await sync()
    }

    func sync() async {
        guard let connections else { return }
        guard !syncing else {
            syncAgain = true
            return
        }
        syncing = true
        defer {
            syncing = false
            if syncAgain {
                syncAgain = false
                Swift.Task { await self.sync() }
            }
        }
        var failed = false
        let deleted = removals
        for server in deleted {
            do {
                try await client(server).deleteDevice(installationID)
                removals.removeAll { $0.id == server.id }
                saveRemovals()
                uploaded.removeValue(forKey: server.id)
            } catch {
                failed = true
                status = "Could not remove a notification registration. Retrying when online."
            }
        }
        for server in connections.servers {
            guard connections.servers.contains(where: { $0.id == server.id }) else { continue }
            if syncedConnections[server.id] != server.connection {
                uploaded.removeValue(forKey: server.id)
                cleared.remove(server.id)
            }
            do {
                let registering = permission.shouldRegister(enabled: enabled)
                if registering, let token {
                    guard let environment else {
                        failed = true
                        status = "The signed app has no APNs entitlement. Install a push-enabled build."
                        continue
                    }
                    let device = DeviceRegistration(id: installationID, token: token, environment: environment, serverId: server.id)
                    guard uploaded[server.id] != device else { continue }
                    try await client(server).registerDevice(device)
                    uploaded[server.id] = device
                    cleared.remove(server.id)
                    syncedConnections[server.id] = server.connection
                } else if !registering, !cleared.contains(server.id) {
                    try await client(server).deleteDevice(installationID)
                    uploaded.removeValue(forKey: server.id)
                    cleared.insert(server.id)
                    syncedConnections[server.id] = server.connection
                }
            } catch {
                failed = true
                status = "Some servers could not update notifications. Retrying when online."
            }
        }
        if !failed { status = nil }
    }

    private func saveRemovals() {
        if let data = try? JSONEncoder().encode(removals) {
            try? removalStore.save(String(decoding: data, as: UTF8.self))
        }
    }

    private let environment: PushEnvironment? = {
        guard let url = Bundle.main.executableURL,
              let data = try? Data(contentsOf: url, options: .mappedIfSafe) else { return nil }
        return PushEnvironment.signedExecutable(data)
    }()

    private func client(_ server: SavedServer) throws -> ServeClient {
        guard let connection = PairingConnection(server.connection),
              let token = connection.token, !token.hasPrefix("pair_") else {
            throw HostError.transport("Pair this server before enabling notifications.")
        }
        return ServeClient(baseURL: connection.baseURL, transport: URLSessionTransport.system(), token: token)
    }

    func open(_ route: NotificationRoute) async {
        guard let connections else {
            pendingRoute = route
            return
        }
        await connections.connectOnLaunch()
        if !(await connections.openNotification(route)) {
            status = connections.failure
            showingSettings = true
        }
    }

    func suppress(_ route: NotificationRoute?) -> Bool {
        guard UIApplication.shared.applicationState == .active, let route, let connections else { return false }
        return route.suppress(activeServerID: connections.activeID, openSessionID: connections.visibleSessionID)
    }
}

@MainActor
final class NotificationAppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        Swift.Task { await NotificationCoordinator.shared.registered(deviceToken) }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: any Error) {
        NotificationCoordinator.shared.status = "Could not register for notifications: " + error.localizedDescription
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping @Sendable (UNNotificationPresentationOptions) -> Void) {
        let route = NotificationRoute(payload: notification.request.content.userInfo)
        Swift.Task { @MainActor in
            completionHandler(NotificationCoordinator.shared.suppress(route) ? [] : [.banner, .list, .sound])
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping @Sendable () -> Void) {
        let route = NotificationRoute(payload: response.notification.request.content.userInfo)
        Swift.Task { @MainActor in
            completionHandler()
            if let route { await NotificationCoordinator.shared.open(route) }
        }
    }
}

struct NotificationSettings: SwiftUI.View {
    @State private var coordinator = NotificationCoordinator.shared

    var body: some SwiftUI.View {
        Form {
            Section("Alert notifications") {
                Text("Get alerts from your paired servers when sessions need attention or finish. No background access is required.")
                if coordinator.enabled {
                    Button("Disable notifications", role: .destructive) {
                        Swift.Task { await coordinator.disable() }
                    }
                } else {
                    Button("Enable notifications") {
                        Swift.Task { await coordinator.enable() }
                    }
                }
                if coordinator.permission == .denied {
                    Button("Open iOS Settings") {
                        if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                    }
                }
                if let status = coordinator.status { Text(status).foregroundStyle(.secondary) }
            }
        }
        .navigationTitle("Notifications")
        .task { await coordinator.refresh() }
    }
}
