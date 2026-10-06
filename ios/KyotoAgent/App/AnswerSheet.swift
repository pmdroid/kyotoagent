import SwiftUI

struct AnswerSheetView: SwiftUI.View {
    @Bindable var model: AppModel
    var sheet: AnswerSheet
    @State private var reply = ""

    var body: some SwiftUI.View {
        NavigationStack {
            List {
                if let notice = model.notice, !notice.isEmpty {
                    Section {
                        Label(notice, systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(Ink.bad)
                            .accessibilityIdentifier("answer-notice")
                    }
                }
                switch sheet {
                case .permission(let permission):
                    permissionSections(permission)
                case .question(let question):
                    questionSections(question)
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle(sheetTitle)
            .navigationBarTitleDisplayMode(.inline)
            .safeAreaInset(edge: .bottom, spacing: 0) {
                if case .question = sheet {
                    replyBar
                }
            }
        }
    }

    private var sheetTitle: String {
        switch sheet {
        case .question:
            "Question"
        case .permission:
            "Permission"
        }
    }

    @ViewBuilder
    private func permissionSections(_ permission: PermissionCard) -> some SwiftUI.View {
        Section {
            Text(permission.action)
                .font(.body)
                .foregroundStyle(Ink.text)
                .textSelection(.enabled)
                .accessibilityIdentifier("answer-action")
            if let path = permission.path, !path.isEmpty {
                LabeledContent("Path") {
                    Text(path)
                        .font(.footnote.monospaced())
                        .multilineTextAlignment(.trailing)
                        .textSelection(.enabled)
                }
                .accessibilityIdentifier("answer-path")
            }
            if let argv = permission.argv, !argv.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Command")
                        .font(.subheadline)
                        .foregroundStyle(Ink.faint)
                    Text(argv.joined(separator: " "))
                        .font(.footnote.monospaced())
                        .foregroundStyle(Ink.text)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .accessibilityIdentifier("answer-argv")
            }
            if let seconds = permission.timeoutSec {
                LabeledContent("Timeout", value: "\(seconds) seconds")
            }
        }
        if let diff = permission.diff, !diff.isEmpty {
            Section("Changes") {
                diffBlock(diff)
            }
        }
        Section {
            decisionRow(
                "Allow Once",
                systemImage: "checkmark",
                choice: Decision.allow_once.rawValue,
                role: nil,
                identifier: "answer-allow-once"
            )
            decisionRow(
                "Allow for Session",
                systemImage: "checkmark.circle",
                choice: Decision.allow_session.rawValue,
                role: nil,
                identifier: "answer-allow-session"
            )
        } footer: {
            Text("Allow for Session remembers this action until the session ends.")
        }
        Section {
            decisionRow(
                "Don’t Allow",
                systemImage: "xmark",
                choice: Decision.deny.rawValue,
                role: .destructive,
                identifier: "answer-deny"
            )
        }
    }

    @ViewBuilder
    private func questionSections(_ question: QuestionCard) -> some SwiftUI.View {
        Section {
            MarkdownBlocksView(blocks: transcriptBlocks(question.text))
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
                .accessibilityIdentifier("answer-prompt")
        }
        if !question.choices.isEmpty {
            Section {
                ForEach(Array(question.choices.enumerated()), id: \.offset) { _, choice in
                    choiceRow(choice, identifier: "answer-choice-" + choice)
                }
            }
        }
    }

    private var replyBar: some SwiftUI.View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField("Reply", text: $reply, axis: .vertical)
                .lineLimit(1...4)
                .textFieldStyle(.plain)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .frame(minHeight: 44)
                .background(Ink.card, in: RoundedRectangle(cornerRadius: 22))
                .accessibilityIdentifier("answer-reply")
            Button("Send reply", systemImage: "arrow.up") {
                sendReply()
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.glassProminent)
            .buttonBorderShape(.circle)
            .controlSize(.regular)
            .frame(minWidth: 44, minHeight: 44)
            .disabled(model.answering || reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityIdentifier("answer-send")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }

    private func choiceRow(_ title: String, identifier: String) -> some SwiftUI.View {
        Button {
            Swift.Task { await model.answer(title) }
        } label: {
            HStack(spacing: 12) {
                Text(title)
                    .font(.body)
                    .foregroundStyle(Ink.accent)
                    .multilineTextAlignment(.leading)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Image(systemName: "chevron.right")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Ink.faint)
            }
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(model.answering)
        .accessibilityIdentifier(identifier)
    }

    private func decisionRow(
        _ title: String,
        systemImage: String,
        choice: String,
        role: ButtonRole?,
        identifier: String
    ) -> some SwiftUI.View {
        Button(role: role) {
            Swift.Task { await model.answer(choice) }
        } label: {
            Label(title, systemImage: systemImage)
                .font(.body)
                .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                .contentShape(Rectangle())
        }
        .disabled(model.answering)
        .accessibilityIdentifier(identifier)
    }

    private func diffBlock(_ diff: [String]) -> some SwiftUI.View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(diff.enumerated()), id: \.offset) { _, line in
                Text(line)
                    .font(.caption.monospaced())
                    .foregroundStyle(diffInk(line))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
        }
        .padding(.vertical, 4)
        .accessibilityIdentifier("answer-diff")
    }

    private func sendReply() {
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            return
        }
        Swift.Task { await model.answer(text) }
    }

    private func diffInk(_ line: String) -> Color {
        if line.hasPrefix("+") {
            return Ink.good
        }
        if line.hasPrefix("-") {
            return Ink.bad
        }
        return Ink.faint
    }
}
