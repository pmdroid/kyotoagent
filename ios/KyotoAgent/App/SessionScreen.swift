import SwiftUI

struct SessionColumn: SwiftUI.View {
    @Bindable var model: AppModel
    @Environment(ServerConnections.self) private var connections
    var openSession: (String) -> Void

    @State private var showNew = false

    var body: some SwiftUI.View {
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
            List(model.nodes) { node in
                SessionRow(
                    line: sessionLine(node.session, depth: node.depth),
                    pull: pullMark(node.session),
                    openPull: { model.openPull(node.session) },
                    openSession: { openSession(node.id) }
                )
                .listRowBackground(node.id == model.selection ? Ink.card : Color.clear)
                .listRowSeparator(.hidden)
                .accessibilityIdentifier("session-row-" + node.id)
                .contextMenu {
                    Button("Delete session", role: .destructive) {
                        model.requestDelete(node.id)
                    }
                    .accessibilityIdentifier("close-" + node.id)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
        }
        .background(Ink.canvas)
        .navigationTitle("Sessions")
        .navigationSubtitle(connections.servers.first { $0.id == connections.activeID }?.title ?? hostLabel(model.baseURLText))
        .phonePopup(isPresented: $showNew) {
            NavigationStack {
                NewSessionScreen(model: model, onClose: { showNew = false })
            }
        }
        .phonePopup(item: pullItem) { pull in
            PullSheet(urlText: pull.url)
        }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                ServerSwitcherButton()
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

    var body: some SwiftUI.View {
        HStack(alignment: .center, spacing: 12) {
            Button(action: openSession) {
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
            .buttonStyle(.plain)
            .frame(maxWidth: .infinity, alignment: .leading)
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
            Button(action: openSession) {
                Text(line.badge)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(badge)
            }
            .buttonStyle(.plain)
        }
        .padding(.leading, CGFloat(line.depth) * 18)
        .padding(.vertical, 4)
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
