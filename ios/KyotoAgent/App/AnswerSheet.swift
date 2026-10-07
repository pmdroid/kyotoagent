import SwiftUI

struct AnswerSheetView: SwiftUI.View {
    @Bindable var model: AppModel
    var sheet: AnswerSheet
    @State private var reply = ""
    @State private var expandedVisual: Int?
    @State private var zoom = 1.0
    @GestureState private var magnification = 1.0

    var body: some SwiftUI.View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                switch sheet {
                case .permission(let permission):
                    permissionBody(permission)
                case .question(let question):
                    questionBody(question)
                }
                if let notice = model.notice, !notice.isEmpty {
                    Text(notice)
                        .font(.subheadline)
                        .foregroundStyle(Ink.bad)
                        .accessibilityIdentifier("answer-notice")
                }
            }
            .padding(.horizontal, 20)
            .padding(.top, 10)
            .padding(.bottom, 24)
        }
        .background(Ink.canvas.opacity(0.72))
        .safeAreaInset(edge: .bottom) {
            if case .question(let question) = sheet, !(question.visuals ?? []).isEmpty {
                VStack(spacing: 8) { questionAnswers(question) }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 10)
                    .background(Ink.canvas)
            }
        }
        .onChange(of: sheet.eventId) {
            reply = ""
            expandedVisual = nil
            zoom = 1
        }
    }

    @ViewBuilder
    private func permissionBody(_ permission: PermissionCard) -> some SwiftUI.View {
        Text(permission.action)
            .font(.title2.bold())
            .foregroundStyle(Ink.text)
            .accessibilityIdentifier("answer-action")
        if let path = permission.path, !path.isEmpty {
            Text(path)
                .font(.caption)
                .foregroundStyle(Ink.faint)
                .accessibilityIdentifier("answer-path")
        }
        if let argv = permission.argv, !argv.isEmpty {
            Text(argv.joined(separator: " "))
                .font(.caption.monospaced())
                .foregroundStyle(Ink.faint)
                .accessibilityIdentifier("answer-argv")
        }
        if let diff = permission.diff, !diff.isEmpty {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(diff.enumerated()), id: \.offset) { _, line in
                    Text(line)
                        .font(.caption.monospaced())
                        .foregroundStyle(diffInk(line))
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .padding(12)
            .background(
                RoundedRectangle(cornerRadius: 14)
                    .fill(Color(red: 0.06, green: 0.09, blue: 0.11))
            )
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(Ink.line, lineWidth: 1))
            .accessibilityIdentifier("answer-diff")
        }
        VStack(spacing: 8) {
            choiceButton(
                "Allow once",
                choice: Decision.allow_once.rawValue,
                tint: Ink.accent,
                fill: Ink.accent.opacity(0.18),
                identifier: "answer-allow-once"
            )
            choiceButton(
                "Allow for session",
                choice: Decision.allow_session.rawValue,
                tint: Ink.text,
                fill: Color.white.opacity(0.06),
                identifier: "answer-allow-session"
            )
            choiceButton(
                "Deny",
                choice: Decision.deny.rawValue,
                tint: Ink.bad,
                fill: Color.white.opacity(0.06),
                identifier: "answer-deny"
            )
        }
    }

    @ViewBuilder
    private func questionBody(_ question: QuestionCard) -> some SwiftUI.View {
        MarkdownBlocksView(blocks: transcriptBlocks(question.text))
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier("answer-prompt")
        ForEach(Array((question.visuals ?? []).enumerated()), id: \.offset) { index, visual in
            visualPreview(visual, index: index)
        }
        if (question.visuals ?? []).isEmpty {
            questionAnswers(question)
        }
    }

    @ViewBuilder
    private func questionAnswers(_ question: QuestionCard) -> some SwiftUI.View {
        if !question.choices.isEmpty {
            VStack(spacing: 8) {
                ForEach(Array(question.choices.enumerated()), id: \.offset) { _, choice in
                    choiceButton(
                        choice,
                        choice: choice,
                        tint: Ink.text,
                        fill: Color.white.opacity(0.06),
                        identifier: "answer-choice-" + choice
                    )
                }
            }
        }
        HStack(alignment: .bottom, spacing: 8) {
            TextField("Reply", text: $reply, axis: .vertical)
                .lineLimit(1...4)
                .foregroundStyle(Ink.text)
                .accessibilityIdentifier("answer-reply")
            Button {
                let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
                Swift.Task { await model.answer(text, eventId: sheet.eventId) }
            } label: {
                Image(systemName: "arrow.up")
                    .font(.body.weight(.bold))
                    .foregroundStyle(Color.black)
                    .frame(width: 36, height: 36)
                    .background(Circle().fill(Ink.accent))
            }
            .buttonStyle(.plain)
            .disabled(model.answering || reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityIdentifier("answer-send")
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 22).fill(Ink.card))
    }

    @ViewBuilder
    private func visualPreview(_ visual: QuestionVisual, index: Int) -> some SwiftUI.View {
        VStack(alignment: .leading, spacing: 8) {
            Text(visual.title)
                .font(.headline)
            if let bytes = visual.image.bytes, let image = UIImage(data: bytes) {
                if expandedVisual == index {
                    ScrollView([.horizontal, .vertical]) {
                        Image(uiImage: image)
                            .resizable()
                            .scaledToFit()
                            .frame(width: max(280, min(image.size.width, 700)) * CGFloat(min(4, max(1, zoom * magnification))))
                            .accessibilityLabel(visual.alt)
                    }
                    .frame(height: 320)
                    .gesture(MagnifyGesture()
                        .updating($magnification) { value, state, _ in state = value.magnification }
                        .onEnded { value in zoom = min(4, max(1, zoom * value.magnification)) })
                    HStack {
                        Button("Zoom in", systemImage: "plus.magnifyingglass") { zoom = min(4, zoom + 0.5) }
                        Button("Zoom out", systemImage: "minus.magnifyingglass") { zoom = max(1, zoom - 0.5) }
                        Button("Collapse") { expandedVisual = nil }
                    }
                    .buttonStyle(.bordered)
                    .frame(minHeight: 44)
                } else {
                    Button {
                        expandedVisual = index
                        zoom = 1
                    } label: {
                        Image(uiImage: image)
                            .resizable()
                            .scaledToFit()
                            .frame(maxWidth: .infinity, maxHeight: 200)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Expand " + visual.title)
                    .accessibilityHint(visual.alt)
                    .accessibilityIdentifier("question-visual-\(index)")
                }
            }
            Text(visual.alt)
                .font(.subheadline)
                .foregroundStyle(Ink.faint)
            if let source = visual.source {
                DisclosureGroup("Diagram source") {
                    Text(verbatim: source)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                }
            }
        }
    }

    private func choiceButton(
        _ title: String,
        choice: String,
        tint: Color,
        fill: Color,
        identifier: String
    ) -> some SwiftUI.View {
        Button {
            Swift.Task { await model.answer(choice, eventId: sheet.eventId) }
        } label: {
            Text(title)
                .font(.body.weight(.semibold))
                .foregroundStyle(tint)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 13)
                .background(RoundedRectangle(cornerRadius: 16).fill(fill))
        }
        .buttonStyle(.plain)
        .disabled(model.answering)
        .accessibilityIdentifier(identifier)
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
