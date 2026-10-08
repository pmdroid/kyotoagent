import SwiftUI

struct RootView: SwiftUI.View {
    @Bindable var model: AppModel
    @Environment(ServerConnections.self) private var connections
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.horizontalSizeClass) private var widthClass
    @State private var column = NavigationSplitViewColumn.sidebar
    @State private var columnVisibility = NavigationSplitViewVisibility.all

    var body: some SwiftUI.View {
        Group {
            if model.connected {
                sessionSplit
            } else {
                ConnectScreen(model: model)
            }
        }
        .onChange(of: model.selection) { _, selection in
            column = selection == nil ? .sidebar : transcriptColumn
        }
        .onChange(of: connections.notificationRevision) { _, _ in
            column = transcriptColumn
        }
        .onChange(of: visibleSessionID, initial: true) { _, id in
            connections.visibleSessionID = id
        }
        .onAppear {
            if model.selection != nil {
                column = transcriptColumn
            }
        }
        .overlay {
            if model.deleteConfirm != nil {
                DeleteConfirmCard(model: model)
            }
        }
        .onKeyPress(phases: .down) { press in
            guard press.key == KeyEquivalent("w"), press.modifiers.contains(.control) else {
                return .ignored
            }
            model.pressDelete()
            return .handled
        }
        .task(id: sessionPoll) {
            guard scenePhase == .active else {
                return
            }
            await connections.pollSessions()
        }
        .task(id: viewPoll) {
            guard openViewPolls(
                connected: model.connected,
                sceneIsActive: scenePhase == .active,
                sessionOpen: model.selection != nil
            ) else {
                return
            }
            while !Swift.Task.isCancelled {
                await model.refreshOpenView()
                try? await Swift.Task.sleep(for: RefreshCadence.view)
            }
        }
    }

    private var sessionSplit: some SwiftUI.View {
        let plan = columnPlan(width: widthClass == .regular ? .pad : .phone, panesHaveRows: panesHaveRows(model.sheets))
        return Group {
            if plan.paneColumn {
                NavigationSplitView(columnVisibility: $columnVisibility, preferredCompactColumn: preferredColumn) {
                    SessionColumn(model: model, openSession: { revealSession($0, $1) })
                        .navigationSplitViewColumnWidth(min: 220, ideal: 268, max: 340)
                } content: {
                    transcript
                } detail: {
                    PaneColumn(model: model)
                        .navigationSplitViewColumnWidth(min: 220, ideal: 250, max: 340)
                }
                .navigationSplitViewStyle(.balanced)
                .background(Ink.canvas)
                .accessibilityIdentifier("columns-3")
            } else {
                NavigationSplitView(preferredCompactColumn: preferredColumn) {
                    SessionColumn(model: model, openSession: { revealSession($0, $1) })
                        .navigationSplitViewColumnWidth(min: 220, ideal: 268, max: 340)
                } detail: {
                    transcript
                }
                .navigationSplitViewStyle(.balanced)
                .background(Ink.canvas)
                .accessibilityIdentifier(plan.columns == 1 ? "columns-1" : "columns-2")
            }
        }
    }

    @ViewBuilder
    private var transcript: some SwiftUI.View {
        if let id = model.selection {
            TranscriptScreen(model: model, sessionId: id)
        } else {
            ContentUnavailableView {
                Label("Sessions", systemImage: "bubble.left.and.bubble.right")
            } description: {
                Text("Select a session")
            }
            .background(Ink.canvas)
        }
    }

    private var preferredColumn: Binding<NavigationSplitViewColumn> {
        Binding(
            get: { splitColumn(preferredCompactSlot(showTranscript: showTranscript, paneColumn: paneColumn)) },
            set: { column = $0 }
        )
    }

    private var visibleSessionID: String? {
        model.connected && (widthClass == .regular || showTranscript) ? model.selection : nil
    }

    private var showTranscript: Bool {
        model.selection != nil && column != .sidebar
    }

    private var paneColumn: Bool {
        columnPlan(width: widthClass == .regular ? .pad : .phone, panesHaveRows: panesHaveRows(model.sheets)).paneColumn
    }

    private func splitColumn(_ slot: CompactSlot) -> NavigationSplitViewColumn {
        switch slot {
        case .sidebar:
            return .sidebar
        case .content:
            return .content
        case .detail:
            return .detail
        }
    }

    private var transcriptColumn: NavigationSplitViewColumn {
        splitColumn(preferredCompactSlot(showTranscript: true, paneColumn: paneColumn))
    }

    private func revealSession(_ serverID: String, _ id: String) {
        guard connections.openSession(serverID: serverID, sessionID: id) else { return }
        let next = compactColumn(after: .session(id), selection: connections.model.selection)
        switch next {
        case .detail:
            column = transcriptColumn
        case .list:
            column = .sidebar
        }
    }

    private var sessionPoll: String {
        "\(scenePhase)-\(connections.servers.map(\.id).joined(separator: ","))"
    }

    private var viewPoll: String {
        "\(model.connected)-\(scenePhase)-\(model.selection ?? "")"
    }
}

struct ConnectScreen: SwiftUI.View {
    @Bindable var model: AppModel
    @Environment(ServerConnections.self) private var connections
    @State private var scanning = false
    @State private var choosingIcon = false
    @State private var showingNotifications = false

    var body: some SwiftUI.View {
        VStack(alignment: .leading, spacing: 22) {
            Text("Kyoto")
                .font(.largeTitle.bold())
                .foregroundStyle(Ink.text)
            if connections.connecting {
                ProgressView("Connecting…")
            }
            Button {
                scanning = true
            } label: {
                Label("Scan pairing code", systemImage: "qrcode.viewfinder")
                    .frame(maxWidth: .infinity, minHeight: 44)
            }
            .disabled(connections.connecting)
            .accessibilityIdentifier("connect-scan")
            Text("Scan the QR code from kyotoagent pair. Your connection is stored in the keychain.")
                .font(.subheadline)
                .foregroundStyle(Ink.faint)
            if let failure = connections.failure ?? model.failure {
                Text(failure)
                    .font(.subheadline)
                    .foregroundStyle(Ink.permission)
                    .accessibilityIdentifier("connect-failure")
                    .textSelection(.enabled)
            }
            if !connections.servers.isEmpty {
                Button("Saved servers", systemImage: "server.rack") {
                    connections.showingServers = true
                }
                .frame(minHeight: 44)
            }
            Button("App icon", systemImage: "app.badge") { choosingIcon = true }
                .frame(minHeight: 44)
            Button("Notifications", systemImage: "bell") { showingNotifications = true }
                .frame(minHeight: 44)
            Spacer()
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Ink.canvas)
        .sheet(isPresented: $showingNotifications) {
            NavigationStack { NotificationSettings() }
        }
        .sheet(isPresented: $choosingIcon) {
            NavigationStack { AppIconPicker() }
                .presentationDragIndicator(.visible)
        }
        .sheet(isPresented: $scanning) {
            PairingScanner { uri in
                scanning = false
                model.baseURLText = uri
                Swift.Task { await connections.connect(model.baseURLText) }
            }
        }
    }

}
