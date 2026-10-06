import SwiftUI

struct ServerSwitcherButton: SwiftUI.View {
    @Environment(ServerConnections.self) private var connections

    var body: some SwiftUI.View {
        Button("Servers", systemImage: "server.rack") {
            connections.showingServers = true
        }
        .accessibilityIdentifier("server-switcher")
    }
}

struct ServerSwitcher: SwiftUI.View {
    @Environment(ServerConnections.self) private var connections
    @Environment(\.dismiss) private var dismiss
    @State private var scanning = false
    @State private var editingServer: SavedServer?
    @State private var serverName = ""
    @State private var renaming = false
    @State private var removing = false

    var body: some SwiftUI.View {
        @Bindable var connections = connections
        NavigationStack {
            Form {
                if !connections.servers.isEmpty {
                    Section("Saved servers") {
                        ForEach(connections.servers) { server in
                            HStack(spacing: 12) {
                                Button {
                                    Swift.Task { await connections.connect(server.connection) }
                                } label: {
                                    HStack {
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text(server.title).foregroundStyle(.primary)
                                            if server.name != nil {
                                                Text(server.address)
                                                    .font(.caption)
                                                    .foregroundStyle(.secondary)
                                            }
                                        }
                                        .multilineTextAlignment(.leading)
                                        Spacer(minLength: 8)
                                        Text(connections.models[server.id]?.connected == true ? "Connected" : "Offline")
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                        if server.id == connections.activeID {
                                            Image(systemName: "checkmark")
                                                .accessibilityLabel("Selected")
                                        }
                                    }
                                    .frame(minHeight: 44)
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                .accessibilityIdentifier("saved-server-" + server.id)
                                Menu {
                                    Button("Rename", systemImage: "pencil") {
                                        editingServer = server
                                        serverName = server.name ?? ""
                                        renaming = true
                                    }
                                    Button("Remove", systemImage: "trash", role: .destructive) {
                                        editingServer = server
                                        removing = true
                                    }
                                } label: {
                                    Image(systemName: "ellipsis.circle")
                                        .frame(width: 44, height: 44)
                                }
                                .accessibilityLabel("Manage " + server.title)
                                .accessibilityIdentifier("manage-server-" + server.id)
                            }
                            .disabled(connections.connecting)
                        }
                    }
                }
                Section("Add server") {
                    Button("Scan pairing code", systemImage: "qrcode.viewfinder") {
                        scanning = true
                    }
                    .disabled(connections.connecting)
                }
                Section("Notifications") {
                    NavigationLink("Notifications") { NotificationSettings() }
                }
                Section("Appearance") {
                    NavigationLink {
                        AppIconPicker()
                    } label: {
                        Label("App icon", systemImage: "app.badge")
                    }
                }
                if connections.connecting {
                    Section {
                        HStack(spacing: 12) {
                            ProgressView()
                            Text("Connecting…")
                        }
                    }
                }
                if let failure = connections.failure {
                    Section {
                        Text(failure)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("server-failure")
                    }
                }
            }
            .navigationTitle("Servers")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                        .disabled(connections.connecting)
                }
            }
            .alert("Rename server", isPresented: $renaming) {
                TextField("Server name", text: $serverName)
                Button("Save") {
                    if let server = editingServer { connections.rename(server.id, name: serverName) }
                }
                Button("Cancel", role: .cancel) {}
            }
            .confirmationDialog("Remove this saved server?", isPresented: $removing, titleVisibility: .visible) {
                Button("Remove server", role: .destructive) {
                    if let server = editingServer { connections.remove(server.id) }
                }
            }
            .sheet(isPresented: $scanning) {
                PairingScanner { uri in
                    scanning = false
                    connections.address = uri
                    Swift.Task { await connections.connect(uri) }
                }
            }
        }
        .interactiveDismissDisabled(connections.connecting)
    }
}
