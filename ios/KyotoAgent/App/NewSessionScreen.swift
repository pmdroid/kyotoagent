import SwiftUI

struct NewSessionScreen: SwiftUI.View {
    var model: AppModel
    @Environment(ServerConnections.self) private var connections
    @State private var selectedServerID: String?
    var onClose: () -> Void = {}

    private var creationModel: AppModel {
        selectedServerID.flatMap { connections.models[$0] } ?? model
    }

    var body: some SwiftUI.View {
        @Bindable var model = creationModel
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                places
                placeSwitch
                profilePicker
                Button {
                    let serverID = selectedServerID ?? connections.activeID
                    Swift.Task {
                        if await model.createSession() {
                            if let serverID { connections.selectServer(serverID) }
                            onClose()
                        }
                    }
                } label: {
                    Text("Create session")
                        .font(.headline)
                        .foregroundStyle(Color.black)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 14)
                        .background(RoundedRectangle(cornerRadius: 16).fill(Ink.accent))
                }
                .buttonStyle(.plain)
                .disabled(!model.canCreate)
                .accessibilityIdentifier("create-session")
                if let notice = model.notice, !notice.isEmpty {
                    Text(notice)
                        .font(.subheadline)
                        .foregroundStyle(Ink.permission)
                        .accessibilityIdentifier("create-notice")
                }
            }
            .padding(20)
        }
        .background(Ink.canvas)
        .navigationTitle("New")
        .navigationSubtitle("Where this session works")
        .navigationBarTitleDisplayMode(.large)
        .toolbarBackground(Ink.canvas, for: .navigationBar)
        .task {
            await connections.loadProjects()
        }
    }

    @ViewBuilder
    private var places: some SwiftUI.View {
        @Bindable var model = creationModel
        if !model.projectsReady {
            ProgressView()
                .tint(Ink.accent)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 28)
        } else if let failure = model.projectFailure {
            Text(failure)
                .font(.subheadline)
                .foregroundStyle(Ink.permission)
                .accessibilityIdentifier("projects-failure")
        } else if model.projects.isEmpty {
            if let server = connections.servers.first(where: { $0.id == (selectedServerID ?? connections.activeID) }) {
                HStack(spacing: 8) {
                    Text("Directory")
                        .font(.headline)
                        .foregroundStyle(Ink.text)
                    ServerBadge(title: server.title)
                }
            }
            TextField("Absolute path", text: $model.workspaceDraft)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .foregroundStyle(Ink.text)
                .padding(16)
                .background(
                    RoundedRectangle(cornerRadius: 16)
                        .stroke(Ink.line, lineWidth: 1)
                )
                .accessibilityIdentifier("workspace-path")
        }
        ForEach(connections.projects) { project in
            projectCard(project)
        }
    }

    private func projectCard(_ item: ServerProject) -> some SwiftUI.View {
        let project = item.project
        let model = creationModel
        let chosen = (selectedServerID ?? connections.activeID) == item.server.id && model.selectedProjectID == project.id
        return Button {
            selectedServerID = item.server.id
            connections.models[item.server.id]?.selectedProjectID = project.id
        } label: {
            HStack(alignment: .center, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 8) {
                        Text(project.name)
                            .font(.headline)
                            .foregroundStyle(Ink.text)
                        ServerBadge(title: item.server.title)
                    }
                    Text(project.path)
                        .font(.subheadline)
                        .foregroundStyle(Ink.faint)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                if chosen {
                    Text("selected")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Ink.accent)
                }
            }
            .padding(16)
            .background(
                RoundedRectangle(cornerRadius: 16)
                    .fill(chosen ? Ink.selected : Ink.card)
            )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("project-" + item.server.id + "-" + project.id)
    }

    private var profilePicker: some SwiftUI.View {
        let model = creationModel
        return VStack(alignment: .leading, spacing: 8) {
            Text("Profile")
                .font(.headline)
                .foregroundStyle(Ink.text)
            ForEach(profileChoices(model.profiles), id: \.self) { name in
                profileButton(name)
            }
        }
    }

    private func profileButton(_ name: String) -> some SwiftUI.View {
        let model = creationModel
        let chosen = name == everythingProfile
            ? createProfileField(model.selectedProfile) == nil
            : model.selectedProfile == name
        return Button {
            model.selectedProfile = createProfileField(name)
        } label: {
            HStack {
                Text(name)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(chosen ? Ink.accent : Ink.text)
                Spacer(minLength: 8)
                if chosen {
                    Text("selected")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Ink.accent)
                }
            }
            .padding(14)
            .background(
                RoundedRectangle(cornerRadius: 14)
                    .fill(chosen ? Ink.selected : Ink.card)
            )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(profileIdentifier(name))
        .accessibilityAddTraits(chosen ? .isSelected : [])
    }

    private func profileIdentifier(_ name: String) -> String {
        if name == everythingProfile {
            return "new-profile-everything"
        }
        return "new-profile-" + name
    }

    private var placeSwitch: some SwiftUI.View {
        let model = creationModel
        return HStack(spacing: 6) {
            placeButton("This folder", selected: !model.useWorktree, id: "place-folder") {
                model.useWorktree = false
            }
            placeButton("Git worktree", selected: model.useWorktree, id: "place-worktree") {
                model.useWorktree = true
            }
        }
        .padding(4)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
    }

    private func placeButton(
        _ title: String,
        selected: Bool,
        id: String,
        action: @escaping () -> Void
    ) -> some SwiftUI.View {
        Button(action: action) {
            Text(title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(selected ? Ink.accent : Ink.faint)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 12)
                .background(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .fill(selected ? Ink.accent.opacity(0.16) : Color.clear)
                )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(id)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

struct PullSheet: SwiftUI.View {
    var urlText: String
    @Environment(\.openURL) private var openURL

    var body: some SwiftUI.View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Pull request")
                .font(.title2.bold())
                .foregroundStyle(Ink.text)
            Text(urlText)
                .font(.subheadline)
                .foregroundStyle(Ink.faint)
                .textSelection(.enabled)
                .accessibilityIdentifier("pull-url")
            Button {
                if let url = safariURL(urlText) {
                    openURL(url)
                }
            } label: {
                Text("Open in Safari")
                    .font(.headline)
                    .foregroundStyle(Color.black)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 14)
                    .background(RoundedRectangle(cornerRadius: 16).fill(Ink.accent))
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("pull-safari")
            Spacer()
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Ink.canvas)
    }
}
