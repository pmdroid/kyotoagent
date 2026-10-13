import SwiftUI

struct SessionColumn: SwiftUI.View {
    @Bindable var model: AppModel
    @Environment(ServerConnections.self) private var connections
    var openSession: (String, String) -> Void

    @State private var showNew = false

    var body: some SwiftUI.View {
        @Bindable var connections = connections
        VStack(spacing: 0) {
            if let notice = model.notice, !notice.isEmpty {
                Text(notice)
                    .font(.subheadline)
                    .foregroundStyle(Ink.permission)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal)
                    .padding(.vertical, 10)
                    .accessibilityIdentifier("session-notice")
            }
            if let notice = connections.sessionDeletionNotice {
                Text(notice)
                    .font(.subheadline)
                    .foregroundStyle(Ink.permission)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal)
                    .accessibilityIdentifier("session-deletion-notice")
            }
            List(selection: Binding(get: { connections.selectedSessions }, set: { connections.updateSelectedSessions($0) })) {
                if !connections.sessionDeletionFailures.isEmpty {
                    Section("Could not delete") {
                        ForEach(connections.sessionDeletionFailures) { failure in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(failure.target.title + " · " + failure.target.serverTitle)
                                    .font(.headline)
                                Text(failure.message)
                                    .font(.subheadline)
                                    .foregroundStyle(Ink.permission)
                            }
                            .selectionDisabled()
                        }
                    }
                }
                Section {
                    Picker("Filter", selection: $connections.sessionFilter) {
                        ForEach(SessionListFilter.allCases) { filter in
                            Text(filter.label).tag(filter)
                        }
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("session-filter")
                    if !connections.selectingSessions {
                        Button(role: .destructive) { connections.requestArchivedSessionDeletion() } label: {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Delete all archived (\(connections.currentServerArchivedCount))")
                                Text("Current server: \(connections.servers.first { $0.id == connections.activeID }?.title ?? "")")
                                    .font(.caption)
                                    .foregroundStyle(Ink.faint)
                            }
                            .frame(minHeight: 44, alignment: .leading)
                        }
                        .disabled(connections.currentServerArchivedCount == 0)
                        .accessibilityIdentifier("delete-archived-sessions")
                    }
                }
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
                ForEach(connections.projectGroups) { group in
                    Section {
                        ForEach(group.nodes) { node in
                            SessionRow(
                                line: sessionLine(node.session, depth: node.depth),
                                pull: connections.selectingSessions ? nil : pullMark(node.session),
                                openPull: {
                                    connections.selectServer(group.server.id)
                                    group.model.openPull(node.session)
                                },
                                openSession: {
                                    if connections.selectingSessions {
                                        connections.toggleSessionSelection(ServerItemID(serverID: group.server.id, itemID: node.id))
                                    } else {
                                        openSession(group.server.id, node.id)
                                    }
                                },
                                selecting: connections.selectingSessions
                            )
                            .tag(ServerItemID(serverID: group.server.id, itemID: node.id))
                            .listRowBackground(group.server.id == connections.activeID && node.id == model.selection ? Ink.card : Color.clear)
                            .listRowSeparator(.hidden)
                            .accessibilityIdentifier("session-row-" + group.server.id + "-" + node.id)
                            .contextMenu {
                                if !connections.selectingSessions {
                                    Button(node.session.archived ? "Unarchive session" : "Archive session") {
                                        connections.selectServer(group.server.id)
                                        Swift.Task {
                                            await group.model.setArchived(node.id, archived: !node.session.archived)
                                        }
                                    }
                                    .accessibilityIdentifier((node.session.archived ? "unarchive-" : "archive-") + node.id)
                                    Button("Delete session", role: .destructive) {
                                        connections.selectServer(group.server.id)
                                        group.model.requestDelete(node.id)
                                    }
                                    .accessibilityIdentifier("close-" + node.id)
                                }
                            }
                        }
                    } header: {
                        HStack(spacing: 8) {
                            Text(group.name)
                                .font(.headline)
                                .foregroundStyle(Ink.text)
                            ServerBadge(title: group.server.title)
                        }
                        .textCase(nil)
                    }
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.editMode, .constant(connections.selectingSessions ? .active : .inactive))
            .disabled(connections.deletingSessions)
            if connections.selectingSessions {
                HStack {
                    Text("\(connections.selectedSessions.count) selected")
                        .font(.headline)
                        .accessibilityIdentifier("session-selection-count")
                    Spacer()
                    if connections.deletingSessions {
                        ProgressView("Deleting sessions")
                    } else {
                        Button("Delete", role: .destructive) { connections.requestSelectedSessionDeletion() }
                            .disabled(connections.selectedSessions.isEmpty)
                            .accessibilityIdentifier("delete-selected-sessions")
                    }
                }
                .padding()
            }
        }
        .background(Ink.canvas)
        .navigationTitle("Sessions")
        .navigationSubtitle("All servers")
        .confirmationDialog(
            connections.sessionDeletion?.title ?? "Delete sessions?",
            isPresented: Binding(get: { connections.sessionDeletion != nil }, set: { if !$0 { connections.cancelSessionDeletion() } }),
            titleVisibility: .visible,
            presenting: connections.sessionDeletion
        ) { confirmation in
            Button("Delete \(confirmation.targets.count) sessions", role: .destructive) {
                Swift.Task { await connections.confirmSessionDeletion(confirmation) }
            }
            Button("Cancel", role: .cancel) { connections.cancelSessionDeletion() }
        } message: { confirmation in
            Text(confirmation.message)
        }
        .phonePopup(isPresented: $showNew) {
            NavigationStack {
                NewSessionScreen(model: model, onClose: { showNew = false })
            }
        }
        .phonePopup(item: pullItem) { pull in
            PullSheet(urlText: pull.url)
        }
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                if connections.selectingSessions {
                    Button("Cancel") { connections.cancelSessionSelection() }
                        .disabled(connections.deletingSessions)
                        .accessibilityIdentifier("cancel-session-selection")
                } else {
                    Button("Select") { connections.beginSessionSelection() }
                        .accessibilityIdentifier("select-sessions")
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                ServerSwitcherButton()
                    .disabled(connections.deletingSessions)
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    showNew = true
                } label: {
                    Image(systemName: "plus")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(Ink.accent)
                        .frame(width: 36, height: 36)
                        .glassEffect(.regular, in: Circle())
                }
                .disabled(connections.selectingSessions || connections.deletingSessions)
                .accessibilityIdentifier("new-session")
            }
        }
    }

    private var pullItem: Binding<ShownPull?> {
        Binding(
            get: {
                guard let pull = model.shownPull else {
                    return nil
                }
                return ShownPull(url: pull)
            },
            set: { next in
                model.shownPull = next?.url
            }
        )
    }

    private func pullMark(_ session: Session) -> String? {
        guard let pull = session.pullUrl, safariURL(pull) != nil else {
            return nil
        }
        return pull
    }


}

private struct ShownPull: Identifiable, Equatable {
    var url: String
    var id: String { url }
}

struct SessionRow: SwiftUI.View {
    var line: SessionLine
    var pull: String?
    var openPull: () -> Void = {}
    var openSession: () -> Void = {}
    var selecting = false

    var body: some SwiftUI.View {
        HStack(alignment: .center, spacing: 12) {
            if selecting {
                sessionLabel
            } else {
                Button(action: openSession) { sessionLabel }
                    .buttonStyle(.plain)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if pull != nil {
                Button(action: openPull) {
                    Text("PR")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(Ink.accent)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .overlay(Capsule().stroke(Ink.accent, lineWidth: 1))
                }
                .buttonStyle(.borderless)
                .accessibilityIdentifier("session-pull")
            }
            if selecting {
                Text(line.badge)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(badge)
            } else {
                Button(action: openSession) {
                    Text(line.badge)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(badge)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.leading, CGFloat(line.depth) * 18)
        .padding(.vertical, 4)
    }

    private var sessionLabel: some SwiftUI.View {
        HStack(alignment: .center, spacing: 12) {
            RoundedRectangle(cornerRadius: 2)
                .fill(bar)
                .frame(width: 3, height: 36)
            VStack(alignment: .leading, spacing: 3) {
                Text(line.name)
                    .font(.headline)
                    .foregroundStyle(Ink.text)
                    .lineLimit(1)
                Text(line.detail)
                    .font(.caption)
                    .foregroundStyle(Ink.faint)
                    .lineLimit(2)
            }
            Spacer(minLength: 8)
        }
        .contentShape(Rectangle())
    }

    private var bar: Color {
        if line.depth > 0, line.status == .working {
            return Ink.accent
        }
        return statusColor(line.status)
    }

    private var badge: Color {
        if line.status == .waiting {
            return Ink.permission
        }
        return statusColor(line.status)
    }
}

struct ServerBadge: SwiftUI.View {
    var title: String

    var body: some SwiftUI.View {
        Text(title)
            .font(.caption)
            .foregroundStyle(Ink.faint)
            .lineLimit(1)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(Ink.card, in: Capsule())
            .accessibilityLabel("Server " + title)
    }
}
