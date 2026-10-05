import SwiftUI

let modelConfirmTitle = "Save as default for all sessions"

struct PaletteSheet: SwiftUI.View {
    @Bindable var model: AppModel
    @State private var query = ""
    @State private var showHelp = false
    @State private var showSchedules = false
    @State private var palettePane: DockPane?

    var body: some SwiftUI.View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    Button {
                        showSchedules = true
                    } label: {
                        Text("Schedules")
                            .font(.headline)
                            .foregroundStyle(Ink.text)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 10)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("palette-schedules")
                    palettePaneButton("Todos", .todos, "palette-todos")
                    palettePaneButton("Closeout", .closeout, "palette-closeout")
                    palettePaneButton("Tasks", .tasks, "palette-tasks")
                    TextField("Filter", text: $query)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .foregroundStyle(Ink.text)
                        .padding(14)
                        .background(RoundedRectangle(cornerRadius: 14).stroke(Ink.line, lineWidth: 1))
                        .accessibilityIdentifier("command-query")
                    ForEach(filteredCatalog(skills: model.transcript.view?.skills ?? [], query: query)) { entry in
                        Button {
                            Swift.Task { await pick(entry) }
                        } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(entry.title)
                                    .font(.headline)
                                    .foregroundStyle(Ink.text)
                                if !entry.hint.isEmpty {
                                    Text(entry.hint)
                                        .font(.caption)
                                        .foregroundStyle(Ink.faint)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 10)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("palette-row-" + entry.id)
                    }
                }
                .padding(20)
            }
            .background(Ink.canvas)
            .navigationTitle("Commands")
            .navigationBarTitleDisplayMode(.inline)
        }
        .phonePopup(isPresented: newSession) {
            NavigationStack {
                NewSessionScreen(model: model, onClose: { model.presentNewSession = false })
            }
        }
        .phonePopup(isPresented: $showHelp) {
            NavigationStack {
                HelpPage(entries: model.helpEntries())
            }
            .accessibilityIdentifier("help-sheet")
        }
        .phonePopup(isPresented: $showSchedules) {
            DockPaneSheet(model: model, pane: .schedules)
        }
        .phonePopup(item: $palettePane) { pane in
            DockPaneSheet(model: model, pane: pane)
        }
        .accessibilityIdentifier("command-palette-sheet")
    }

    private var newSession: Binding<Bool> {
        Binding(
            get: { model.presentNewSession },
            set: { model.presentNewSession = $0 }
        )
    }

    private func palettePaneButton(_ title: String, _ pane: DockPane, _ identifier: String) -> some SwiftUI.View {
        Button {
            palettePane = pane
        } label: {
            Text(title)
                .font(.headline)
                .foregroundStyle(Ink.text)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 10)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(identifier)
    }

    private func pick(_ entry: CommandEntry) async {
        let result = await model.runPalette(entry.kind)
        if result == .showHelp {
            showHelp = true
        }
    }
}

struct HelpPage: SwiftUI.View {
    var entries: [CommandEntry]

    var body: some SwiftUI.View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                ForEach(entries) { entry in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.title)
                            .font(.headline)
                            .foregroundStyle(Ink.text)
                        if !entry.hint.isEmpty {
                            Text(entry.hint)
                                .font(.subheadline)
                                .foregroundStyle(Ink.faint)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier("help-row-" + entry.id)
                }
            }
            .padding(20)
        }
        .background(Ink.canvas)
        .navigationTitle("Help")
        .navigationBarTitleDisplayMode(.inline)
        .accessibilityIdentifier("help-page")
    }
}

struct ModelSheet: SwiftUI.View {
    @Bindable var model: AppModel
    @State private var chosenID = ""
    @State private var chosenProvider: String?
    @State private var applying = false
    @State private var chosenEffort: String?

    var body: some SwiftUI.View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach(model.models, id: \.self) { row in
                        Button {
                            guard !applying, !model.savingModel else { return }
                            select(row)
                            applySelection()
                        } label: {
                            modelRow(row)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("model-row-" + row.id)
                    }
                    if !efforts.isEmpty {
                        Text("Effort")
                            .font(.headline)
                            .foregroundStyle(Ink.text)
                        ForEach(efforts, id: \.self) { level in
                            Button {
                                guard !applying, !model.savingModel else { return }
                                chosenEffort = level
                                applySelection()
                            } label: {
                                effortLabel(level, chosen: chosenEffort == level)
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("model-effort-" + level)
                        }
                    }

                }
                .padding(20)
                .disabled(model.savingModel || applying)
            }
            .background(Ink.canvas)
            .safeAreaInset(edge: .bottom) {
                VStack(spacing: 10) {
                    if let notice = model.notice, !notice.isEmpty {
                        Text(notice)
                            .font(.callout)
                            .foregroundStyle(.red)
                            .textSelection(.enabled)
                    }
                    if model.savingModel {
                        ProgressView("Switching model…")
                    } else {
                        Text("Tap a model to use it in this session.")
                            .font(.caption)
                            .foregroundStyle(Ink.faint)
                    }
                    Button {
                        Swift.Task { await model.saveModel(chosenID, effort: chosenEffort, provider: chosenProvider, saveAsDefault: true) }
                    } label: {
                        Text(modelConfirmTitle)
                    }
                    .disabled(chosenID.isEmpty || model.savingModel || applying)
                    .accessibilityIdentifier("model-save-default")
                }
                .frame(maxWidth: .infinity)
                .padding(16)
                .background(Ink.canvas)
            }
            .navigationTitle("Model")
            .navigationBarTitleDisplayMode(.inline)
        }
        .onAppear(perform: seed)
        .accessibilityIdentifier("model-sheet")
    }

    private var efforts: [String] {
        model.models.first { $0.id == chosenID && $0.provider == chosenProvider }?.reasoning_efforts ?? []
    }

    private func seed() {
        let current = model.sessions.first { $0.id == model.selection }
        let id = current?.model ?? model.models.first?.id ?? ""
        chosenID = id
        chosenProvider = model.models.first { $0.id == id && $0.provider == current?.provider }?.provider
        let choices = efforts
        if let effort = current?.effort, choices.contains(effort) {
            chosenEffort = effort
        } else {
            chosenEffort = nil
        }
    }

    private func select(_ row: Model) {
        chosenID = row.id
        chosenProvider = row.provider
        let choices = row.reasoning_efforts
        if choices.isEmpty || !choices.contains(chosenEffort ?? "") {
            chosenEffort = nil
        }
    }

    private func applySelection() {
        applying = true
        let id = chosenID
        let effort = chosenEffort
        let provider = chosenProvider
        Swift.Task {
            await model.saveModel(id, effort: effort, provider: provider, dismiss: false)
            seed()
            applying = false
        }
    }

    private func modelRow(_ row: Model) -> some SwiftUI.View {
        let chosen = row.id == chosenID && row.provider == chosenProvider
        return HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(row.id)
                    .font(.headline)
                    .foregroundStyle(Ink.text)
                if let provider = row.provider {
                    Text(provider)
                        .font(.caption)
                        .foregroundStyle(Ink.faint)
                }
            }
            Spacer()
            if chosen {
                Text("Selected")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Ink.accent)
            }
        }
        .padding(14)
        .background(RoundedRectangle(cornerRadius: 14).fill(chosen ? Ink.selected : Ink.card))
    }

    private func effortLabel(_ level: String, chosen: Bool) -> some SwiftUI.View {
        Text(level)
            .font(.body.weight(.semibold))
            .foregroundStyle(chosen ? Color.black : Ink.text)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14)
            .background(RoundedRectangle(cornerRadius: 14).fill(chosen ? Ink.accent : Ink.card))
    }
}

struct ProfileSheet: SwiftUI.View {
    @Bindable var model: AppModel

    var body: some SwiftUI.View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(profileChoices(model.profiles), id: \.self) { name in
                        Button {
                            Swift.Task { await model.applyProfile(name) }
                        } label: {
                            profileRow(name)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier(profileIdentifier(name))
                    }
                }
                .padding(20)
            }
            .background(Ink.canvas)
            .navigationTitle("Profile")
            .navigationBarTitleDisplayMode(.inline)
        }
        .accessibilityIdentifier("profile-sheet")
    }

    private func profileRow(_ name: String) -> some SwiftUI.View {
        let current = model.sessions.first { $0.id == model.selection }?.profile
        let chosen = liveProfileField(name) == (current ?? "")
        return HStack {
            Text(name)
                .font(.body.weight(.semibold))
                .foregroundStyle(chosen ? Color.black : Ink.text)
            Spacer(minLength: 8)
        }
        .padding(14)
        .background(RoundedRectangle(cornerRadius: 14).fill(chosen ? Ink.accent : Ink.card))
    }

    private func profileIdentifier(_ name: String) -> String {
        if name == everythingProfile {
            return "profile-everything"
        }
        return "profile-row-" + name
    }
}

struct EffortSheet: SwiftUI.View {
    @Bindable var model: AppModel

    var body: some SwiftUI.View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(efforts, id: \.self) { level in
                        Button {
                            Swift.Task { await model.saveModel(modelID, effort: level, provider: model.sessions.first { $0.id == model.selection }?.provider) }
                        } label: {
                            Text(level)
                                .font(.body.weight(.semibold))
                                .foregroundStyle(level == currentEffort ? Color.black : Ink.text)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(14)
                                .background(
                                    RoundedRectangle(cornerRadius: 14)
                                        .fill(level == currentEffort ? Ink.accent : Ink.card)
                                )
                        }
                        .buttonStyle(.plain)
                        .disabled(model.savingModel)
                        .accessibilityIdentifier("effort-row-" + level)
                    }
                }
                .padding(20)
            }
            .background(Ink.canvas)
            .navigationTitle("Effort")
            .navigationBarTitleDisplayMode(.inline)
        }
        .accessibilityIdentifier("effort-sheet")
    }

    private var modelID: String {
        model.sessions.first { $0.id == model.selection }?.model ?? ""
    }

    private var currentEffort: String? {
        model.sessions.first { $0.id == model.selection }?.effort
    }

    private var efforts: [String] {
        let provider = model.sessions.first { $0.id == model.selection }?.provider
        let row = model.models.first { $0.id == modelID && $0.provider == provider }
            ?? model.models.first { $0.id == modelID && $0.provider == nil }
        return row?.reasoning_efforts ?? []
    }
}
