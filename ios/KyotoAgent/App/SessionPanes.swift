import SwiftUI

enum DockPane: String, Identifiable {
    case todos
    case closeout
    case tasks
    case schedules

    var id: String { rawValue }

    var title: String {
        switch self {
        case .todos:
            return "Todos"
        case .closeout:
            return "Closeout"
        case .tasks:
            return "Tasks"
        case .schedules:
            return "Schedules"
        }
    }
}

struct ShownFile: Identifiable, Equatable {
    var path: String
    var id: String { path }
}

struct PaneColumn: SwiftUI.View {
    @Bindable var model: AppModel

    var body: some SwiftUI.View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 6) {
                    if let sheets = model.sheets {
                        if !sheets.todos.isEmpty {
                            columnHeader("Todos", id: "todos")
                            todoLinks
                        }
                        if !sheets.closeout.isEmpty {
                            columnHeader("Closeout", id: "closeout")
                            CloseoutNotice(model: model)
                            closeoutLinks
                        }
                        if !sheets.tasks.isEmpty {
                            columnHeader("Tasks", id: "tasks")
                            taskLinks
                        }
                        if !sheets.schedules.isEmpty {
                            columnHeader("Schedules", id: "schedules")
                            scheduleRows
                        }
                    }
                }
                .padding(.horizontal, 12)
                .padding(.bottom, 16)
            }
            .background(Ink.canvas)
        }
        .accessibilityIdentifier("pane-column")
    }

    private func columnHeader(_ title: String, id: String) -> some SwiftUI.View {
        Text(title.uppercased())
            .font(.caption.weight(.bold))
            .foregroundStyle(Ink.faint)
            .padding(.top, 14)
            .padding(.horizontal, 4)
            .accessibilityIdentifier("pane-" + id)
    }
}

struct DockPaneSheet: SwiftUI.View {
    @Bindable var model: AppModel
    var pane: DockPane

    var body: some SwiftUI.View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 6) {
                    switch pane {
                    case .todos:
                        todoLinks
                    case .closeout:
                        if !(model.sheets?.closeout.isEmpty ?? true) {
                            CloseoutNotice(model: model)
                        }
                        closeoutLinks
                    case .tasks:
                        taskLinks
                    case .schedules:
                        scheduleRows
                    }
                }
                .padding(16)
            }
            .background(Ink.canvas)
            .navigationTitle(pane.title)
            .navigationBarTitleDisplayMode(.inline)
        }
        .accessibilityIdentifier("dock-sheet-" + pane.rawValue)
    }
}

private struct CloseoutNotice: SwiftUI.View {
    @Bindable var model: AppModel

    var body: some SwiftUI.View {
        if model.sheets?.closeoutBypassed == true {
            VStack(alignment: .leading, spacing: 8) {
                Text("Failed closeout accepted for this session")
                    .font(.subheadline.weight(.semibold))
                    .accessibilityIdentifier("closeout-accepted")
                Text("Checks are skipped. Enabling closeout starts a fresh retry budget and keeps previous failures.")
                    .font(.subheadline)
                    .foregroundStyle(Ink.faint)
                Button("Enable closeout") {
                    Swift.Task { await model.enableCloseout() }
                }
                .frame(minHeight: 44)
                .disabled(!model.connected || model.sending || model.displayedStatus != .idle)
                .accessibilityIdentifier("closeout-enable")
            }
            .foregroundStyle(Ink.text)
        } else {
            Text("Closeout checks are required")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Ink.text)
                .accessibilityIdentifier("closeout-required")
        }
    }
}

private extension PaneColumn {
    var todoLinks: some SwiftUI.View { PaneLinks(model: model).todos }
    var closeoutLinks: some SwiftUI.View { PaneLinks(model: model).closeout }
    var taskLinks: some SwiftUI.View { PaneLinks(model: model).tasks }
    var scheduleRows: some SwiftUI.View { PaneLinks(model: model).schedules }
}

private extension DockPaneSheet {
    var todoLinks: some SwiftUI.View { PaneLinks(model: model).todos }
    var closeoutLinks: some SwiftUI.View { PaneLinks(model: model).closeout }
    var taskLinks: some SwiftUI.View { PaneLinks(model: model).tasks }
    var scheduleRows: some SwiftUI.View { PaneLinks(model: model).schedules }
}

struct PaneLinks {
    @Bindable var model: AppModel

    var todos: some SwiftUI.View {
        ForEach(model.sheets?.todos ?? []) { row in
            NavigationLink {
                TodoDetail(model: model, id: row.id)
            } label: {
                paneCard(row.title, row.status.rawValue)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("todo-row-" + row.id)
        }
    }

    var closeout: some SwiftUI.View {
        ForEach(model.sheets?.closeout ?? []) { row in
            NavigationLink {
                CloseoutDetail(model: model, id: row.id)
            } label: {
                VStack(alignment: .leading, spacing: 8) {
                    paneCard(row.id, closeoutSummary(row, bypassed: model.sheets?.closeoutBypassed ?? false))
                    if !row.tail.isEmpty {
                        Text(row.tail)
                            .font(.caption.monospaced())
                            .foregroundStyle(Ink.text)
                            .lineLimit(8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .accessibilityIdentifier("closeout-output-" + row.id)
                    }
                }
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("closeout-row-" + row.id)
        }
    }

    var tasks: some SwiftUI.View {
        ForEach(model.sheets?.tasks ?? []) { row in
            NavigationLink {
                TaskDetailScreen(model: model, id: row.id)
            } label: {
                paneCard(row.id, row.state.rawValue)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("task-row-" + row.id)
        }
    }

    var schedules: some SwiftUI.View {
        ForEach(model.sheets?.schedules ?? []) { row in
            paneCard(row.id + " · " + row.note, String(row.remainingMin))
                .accessibilityIdentifier("schedule-row-" + row.id)
        }
    }

    private func paneCard(_ title: String, _ detail: String) -> some SwiftUI.View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Ink.text)
            Text(detail)
                .font(.caption)
                .foregroundStyle(Ink.faint)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.white.opacity(0.03)))
    }
}

struct TodoDetail: SwiftUI.View {
    @Bindable var model: AppModel
    var id: String
    @State private var file: ShownFile?

    var body: some SwiftUI.View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let description = row?.description, !description.isEmpty {
                    Text(description)
                        .foregroundStyle(Ink.text)
                }
                ForEach(row?.files ?? [], id: \.self) { path in
                    Button(path) {
                        file = ShownFile(path: path)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Ink.accent)
                    .accessibilityIdentifier("todo-file")
                }
                ForEach(row?.links ?? [], id: \.self) { link in
                    linkRow(link)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Ink.canvas)
        .navigationTitle(row?.title ?? id)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .phonePopup(item: $file) { shown in
            NavigationStack {
                FileDetail(model: model, path: shown.path)
            }
            .accessibilityIdentifier("file-sheet")
        }
    }

    private var row: TodoRow? {
        model.sheets?.todos.first { $0.id == id }
    }

    @ViewBuilder
    private func linkRow(_ link: String) -> some SwiftUI.View {
        if let url = safariURL(link) {
            Link(destination: url) {
                Text(link)
                    .foregroundStyle(Ink.accent)
            }
            .accessibilityIdentifier("todo-link")
        } else {
            Text(link)
                .foregroundStyle(Ink.faint)
        }
    }
}

struct FileDetail: SwiftUI.View {
    @Bindable var model: AppModel
    var path: String

    var body: some SwiftUI.View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let file = model.openedFile, file.path == path {
                    Text(file.text)
                        .font(.body.monospaced())
                        .foregroundStyle(Ink.text)
                        .textSelection(.enabled)
                        .accessibilityIdentifier("file-text")
                    if file.truncated {
                        Text("truncated")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(Ink.permission)
                            .accessibilityIdentifier("file-truncated")
                    }
                }
                if let failure = model.fileFailure {
                    Text(failure)
                        .foregroundStyle(Ink.permission)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Ink.canvas)
        .navigationTitle(path)
        .task { await model.openFile(path) }
    }
}

struct CloseoutDetail: SwiftUI.View {
    @Bindable var model: AppModel
    var id: String

    var body: some SwiftUI.View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let row {
                    Text(closeoutRequirement(row, bypassed: model.sheets?.closeoutBypassed ?? false))
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Ink.faint)
                }
                Text(row?.tail ?? "")
                .font(.body.monospaced())
                .foregroundStyle(Ink.text)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier("closeout-tail")
                ForEach(Array(latestRuns.enumerated()), id: \.offset) { _, run in
                    Text(closeoutAttemptSummary(run))
                        .font(.caption.weight(.semibold))
                        .foregroundStyle((run.passed ?? (run.exit == 0)) && run.timed_out != true ? Ink.text : Ink.bad)
                    if let file = run.transcript, let session = model.selection {
                        ArtifactCardView(file: file, download: { try await model.artifact($0, session: session) })
                    }
                }
            }
            .padding()
        }
        .background(Ink.canvas)
        .navigationTitle(id)
    }

    private var row: CloseoutRow? {
        model.sheets?.closeout.first { $0.id == id }
    }

    private var latestRuns: [CloseoutAttempt] {
        row?.status == .running ? [] : Array((row?.runs ?? []).suffix(1))
    }
}

struct TaskDetailScreen: SwiftUI.View {
    @Bindable var model: AppModel
    var id: String

    var body: some SwiftUI.View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let detail = model.openedTask, detail.id == id {
                    Text(detail.argv.joined(separator: " "))
                        .font(.body.monospaced())
                        .foregroundStyle(Ink.text)
                    Text(detail.state.rawValue)
                        .foregroundStyle(Ink.faint)
                    Text(detail.exit.map(String.init) ?? "")
                        .foregroundStyle(Ink.faint)
                    Text(detail.tail)
                        .font(.body.monospaced())
                        .foregroundStyle(Ink.text)
                        .textSelection(.enabled)
                        .accessibilityIdentifier("task-tail")
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Ink.canvas)
        .navigationTitle(id)
        .task { await model.openTask(id) }
        .onDisappear { model.closeTask() }
    }
}

struct ThinkingSheet: SwiftUI.View {
    @Bindable var model: AppModel

    var body: some SwiftUI.View {
        ScrollView {
            Text(model.thinkingBody)
                .foregroundStyle(Ink.text)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding()
                .accessibilityIdentifier("thinking-text")
        }
        .background(Ink.canvas)
    }
}

struct ContextSheetView: SwiftUI.View {
    var sheet: ContextSheet

    var body: some SwiftUI.View {
        NavigationStack {
            List {
                Section {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("\(sheet.percent)% used")
                            .font(.title2.weight(.semibold))
                            .monospacedDigit()
                        ProgressView(value: Double(min(max(sheet.percent, 0), 100)), total: 100)
                            .accessibilityLabel("Context used")
                            .accessibilityValue("\(sheet.percent) percent")
                    }
                    .padding(.vertical, 8)
                    LabeledContent("Estimated used", value: "\(sheet.used.formatted()) tokens")
                        .accessibilityIdentifier("context-used")
                    if let window = sheet.window {
                        LabeledContent("Capacity", value: "\(window.formatted()) tokens")
                            .accessibilityIdentifier("context-window")
                        LabeledContent("Available", value: "\(max(0, window - sheet.used).formatted()) tokens")
                    }
                } header: {
                    Text("Context window")
                } footer: {
                    Text("Estimated from the current conversation, instructions, and tool definitions.")
                }
                if let reported = sheet.reportedPromptTokens {
                    Section {
                        LabeledContent("Input tokens", value: reported.formatted())
                            .accessibilityIdentifier("context-last-request")
                    } header: {
                        Text("Last request")
                    } footer: {
                        Text("Usage reported by the provider for the last model request.")
                    }
                }
                Section("What uses context") {
                    ForEach(sheet.buckets.filter { $0.id != .free }) { bucket in
                        VStack(alignment: .leading, spacing: 8) {
                            LabeledContent(bucketTitle(bucket.id)) {
                                Text(bucket.tokens.map { $0.formatted() + " tokens" } ?? "Not reported")
                                    .monospacedDigit()
                            }
                            if let tokens = bucket.tokens, let window = sheet.window, window > 0 {
                                ProgressView(value: Double(min(max(0, tokens), window)), total: Double(window))
                                    .accessibilityHidden(true)
                            }
                        }
                        .padding(.vertical, 4)
                    }
                }
                .accessibilityIdentifier("context-buckets")
            }
            .navigationTitle("Context")
            .navigationBarTitleDisplayMode(.inline)
        }
    }

    private func bucketTitle(_ id: BucketId) -> String {
        switch id {
        case .system: "System instructions"
        case .tools: "Tool definitions"
        case .skills: "Skills"
        case .messages: "Conversation"
        case .free: "Available"
        }
    }
}
