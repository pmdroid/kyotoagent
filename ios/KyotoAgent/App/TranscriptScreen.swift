import SwiftUI
#if canImport(UIKit)
import UIKit
#endif

struct TranscriptScreen: SwiftUI.View {
    @Bindable var model: AppModel
    var sessionId: String
    @State private var expandedCard: Card?
    @State private var pastedText: String?
    @State private var thinkingOpen = false
    @State private var contextOpen = false
    @FocusState private var composerFocused: Bool
    @State private var goalOpen = false
    @State private var artifactsOpen = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some SwiftUI.View {
        VStack(spacing: 0) {
            if !bar.under.isEmpty {
                header
            }
            if let goal = model.transcript.view?.goal {
                goalButton(goal)
            }
            TranscriptCards(
                cards: model.transcript.view?.cards ?? [],
                revision: model.transcript.view?.revision ?? 0,
                onOpenCommands: { model.openPalette() },
                onOpenPermission: { model.reopenPermission($0) },
                onOpenText: { expandedCard = $0 },
                onOpenImage: model.previewImage,
                onOpenArtifact: { file in try await model.artifact(file, session: sessionId) },
                onEnhance: { choice, text in
                    Swift.Task { await model.answerEnhance(choice, text: text) }
                }
            )
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            composer
        }
        .background(Ink.canvas)
        .navigationTitle(bar.title)
        .navigationBarTitleDisplayMode(.inline)
        .phonePopup(item: cover) { item in
            coverPage(item)
        }
        .onKeyPress(.escape) {
            handleEscape()
        }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Commands", systemImage: "slider.horizontal.3") {
                    ComposerKeyboard.dismiss()
                    model.openPalette()
                }
                .accessibilityIdentifier("chat-commands")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button("Artifacts") { artifactsOpen = true }
                    .accessibilityIdentifier("chat-artifacts")
            }
            ToolbarItem(placement: .topBarTrailing) {
                ServerSwitcherButton()
            }
            if model.displayedStatus == .working {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Cancel") {
                        guard hardwareKeyAction(
                            key: .controlX,
                            popupOpen: shownCover != nil,
                            enhanceOpen: model.enhanceCard != nil
                        ) == .cancelTurn else {
                            return
                        }
                        Swift.Task { await model.cancelTurn() }
                    }
                    .keyboardShortcut("x", modifiers: .control)
                    .accessibilityIdentifier("cancel-turn")
                }
            }
        }
    }

    private func goalButton(_ goal: Goal) -> some SwiftUI.View {
        Button { goalOpen = true } label: {
            HStack(spacing: 12) {
                Image(systemName: "target")
                    .foregroundStyle(goalColor(goal.status))
                VStack(alignment: .leading, spacing: 3) {
                    Text("Goal · " + goal.status.title)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(goalColor(goal.status))
                    Text(goal.objective)
                        .font(.subheadline)
                        .foregroundStyle(Ink.text)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Ink.faint)
            }
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.horizontal)
        .padding(.vertical, 6)
        .background(Ink.card)
        .accessibilityIdentifier("goal-status")
    }

    private var goalPage: some SwiftUI.View {
        NavigationStack {
            ScrollView {
                if let goal = model.transcript.view?.goal {
                    VStack(alignment: .leading, spacing: 16) {
                        Text(goal.status.title)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(goalColor(goal.status))
                        Text(goal.objective)
                            .font(.headline)
                            .textSelection(.enabled)
                        Text(goal.tokens_used.formatted() + " tokens" + (goal.token_budget.map { " of " + $0.formatted() } ?? ""))
                            .font(.subheadline)
                            .foregroundStyle(Ink.faint)
                        if !goal.verification.isEmpty {
                            Text(goal.verification)
                                .font(.body)
                                .textSelection(.enabled)
                                .accessibilityIdentifier("goal-verification")
                        }
                        HStack {
                            if goal.status == .active {
                                Button("Pause", systemImage: "pause") { postGoal("pause") }
                            }
                            if goal.status == .paused {
                                Button("Resume", systemImage: "play") { postGoal("resume") }
                            }
                            Spacer()
                            Button("Clear", role: .destructive) { postGoal("clear") }
                        }
                        .buttonStyle(.bordered)
                        .disabled(model.sending)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
                }
            }
            .background(Ink.canvas)
            .foregroundStyle(Ink.text)
            .navigationTitle("Goal")
            .navigationBarTitleDisplayMode(.inline)

        }
    }

    private func postGoal(_ command: String) {
        Swift.Task {
            await model.goalCommand(command)
            goalOpen = false
        }
    }

    private func goalColor(_ status: GoalStatus) -> Color {
        switch status {
        case .active: Ink.accent
        case .paused, .budget_exhausted: Ink.permission
        case .complete: Ink.good
        }
    }

    private func handleEscape() -> KeyPress.Result {
        switch hardwareKeyAction(
            key: .escape,
            popupOpen: shownCover != nil,
            enhanceOpen: model.enhanceCard != nil
        ) {
        case .dismissPopup:
            releaseCover()
            return .handled
        case .discardEnhance:
            let posted = enhanceAnswer(.discard)
            Swift.Task { await model.answerEnhance(posted.choice, text: posted.text) }
            return .handled
        case .cancelTurn, nil:
            return .ignored
        }
    }

    private var header: some SwiftUI.View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            if let session = model.sessions.first(where: { $0.id == sessionId }),
               let name = headerProfileWord(session) {
                Text(name)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Ink.text)
                    .accessibilityIdentifier("header-profile")
            }

        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal)
        .padding(.vertical, 6)
    }

    private var bar: TranscriptBar {
        let session = model.sessions.first(where: { $0.id == sessionId })
        let name = session.map(sessionName) ?? String(sessionId.prefix(4))
        let profile = session.flatMap(headerProfileWord)
        return transcriptBar(
            name: name,
            phase: nil,
            percent: nil,
            yolo: nil,
            profile: profile,
            badge: nil
        )
    }

    private var activityRow: some SwiftUI.View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 12) {
                workingButton
                yoloLabel
                Spacer(minLength: 8)
                contextButton
            }
            VStack(alignment: .leading, spacing: 0) {
                workingButton
                yoloLabel
                contextButton
            }
        }
        .font(.caption)
        .padding(.horizontal, 4)
    }

    private var yoloWord: String? {
        model.sessions.first(where: { $0.id == sessionId }).flatMap(headerYoloWord)
    }

    @ViewBuilder
    private var yoloLabel: some SwiftUI.View {
        if let word = yoloWord {
            Text(word.uppercased())
                .font(.caption.weight(.bold))
                .foregroundStyle(LinearGradient(
                    colors: [.red, .orange, .green, .blue, .purple],
                    startPoint: .leading,
                    endPoint: .trailing
                ))
                .fixedSize()
                .accessibilityLabel("YOLO mode enabled")
                .accessibilityIdentifier("activity-yolo")
        }
    }

    @ViewBuilder
    private var workingButton: some SwiftUI.View {
        if model.displayedStatus == .working {
            Button { thinkingOpen = true } label: {
                HStack(spacing: 8) {
                    if reduceMotion {
                        Image(systemName: "ellipsis")
                    } else {
                        ProgressView().controlSize(.small)
                    }
                    Text(model.phaseLabel ?? "Working")
                }
                .frame(minHeight: 44)
                .accessibilityIdentifier("agent-working")
            }
            .buttonStyle(.plain)
            .foregroundStyle(Ink.faint)
            .accessibilityLabel("Kyoto is working. Show activity details")
            .accessibilityIdentifier("phase-button")
        }
    }

    @ViewBuilder
    private var contextButton: some SwiftUI.View {
        if let percent = model.contextSheet?.percent {
            Button { contextOpen = true } label: {
                HStack(spacing: 6) {
                    Image(systemName: "chart.pie")
                    Text("Context · \(percent)%")
                        .monospacedDigit()
                    Image(systemName: "chevron.up")
                        .imageScale(.small)
                }
                .frame(minHeight: 44)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Context, \(percent) percent used. Show details")
            .accessibilityIdentifier("context-percent")
        }
    }

    private var composer: some SwiftUI.View {
        VStack(alignment: .leading, spacing: 8) {
            if model.displayedStatus == .working || model.contextSheet != nil || yoloWord != nil {
                activityRow
            }
            skillSuggestions
            if let failure = model.transcriptFailure {
                Text(failure)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("transcript-refresh-failure")
            }
            if let notice = model.notice, !notice.isEmpty {
                Text(notice)
                    .font(.caption)
                    .foregroundStyle(Ink.permission)
            }
            if let count = model.transcript.view?.queue.count, count > 0 {
                Text("queued \(count)")
                    .font(.caption)
                    .foregroundStyle(Ink.faint)
                    .accessibilityIdentifier("composer-queue")
            }
            VStack(alignment: .leading, spacing: 8) {
                if let pasted = model.pastedInput {
                    HStack(spacing: 8) {
                        Button { pastedText = pasted } label: {
                            Label("Pasted input: \(pasted.count) chars", systemImage: "doc.text")
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(Ink.accent)
                        .accessibilityIdentifier("pasted-input")
                        Button("Remove pasted input", systemImage: "xmark") { model.removePastedInput() }
                            .labelStyle(.iconOnly)
                            .buttonStyle(.plain)
                            .foregroundStyle(Ink.faint)
                            .accessibilityIdentifier("remove-pasted-input")
                    }
                    .font(.subheadline)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                }
                ImageAttachmentStrip(images: model.images, onOpen: model.previewImage, onRemove: model.removeImage)
                GlassEffectContainer(spacing: 8) {
                    HStack(alignment: .bottom, spacing: 8) {
                        ImageAttachmentsView(model: model)
                        TextField(
                            "Message Kyoto",
                            text: Binding(
                                get: { model.composerDraft },
                                set: { model.updateComposerDraft($0) }
                            ),
                            axis: .vertical
                        )
                        .lineLimit(1...6)
                        .textFieldStyle(.plain)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .background(Ink.card, in: RoundedRectangle(cornerRadius: 22))
                        .focused($composerFocused)
                        .accessibilityIdentifier("composer-draft")
                        Button("Send", systemImage: "arrow.up") {
                            Swift.Task { await model.send() }
                        }
                        .labelStyle(.iconOnly)
                        .buttonStyle(.glassProminent)
                        .buttonBorderShape(.circle)
                        .controlSize(.regular)
                        .frame(minWidth: 44, minHeight: 44)
                        .disabled(sendDisabled)
                        .accessibilityIdentifier("composer-send")
                    }
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 8)
        .background(.bar)
    }

    @ViewBuilder
    private var skillSuggestions: some SwiftUI.View {
        let matches = skillMatches(model.transcript.view?.skills ?? [], draft: model.draft)
        if !matches.isEmpty {
            ScrollView(.horizontal) {
                HStack(spacing: 8) {
                    ForEach(matches, id: \.name) { skill in
                        Button("/" + skill.name) {
                            model.updateDraft(filledSkill(skill.name))
                        }
                        .buttonStyle(.glass)
                        .accessibilityIdentifier("skill-match-" + skill.name)
                    }
                }
                .padding(.vertical, 4)
            }
            .scrollIndicators(.hidden)
        }
    }

    private var cover: Binding<ShownCover?> {
        Binding(
            get: { shownCover },
            set: { next in
                guard next == nil else {
                    return
                }
                releaseCover()
            }
        )
    }

    private var shownCover: ShownCover? {
        if let image = model.shownImage { return .image(image) }
        if let pastedText { return .pasted(pastedText) }
        if let expandedCard { return .text(expandedCard) }
        if goalOpen { return .goal }
        if artifactsOpen { return .artifacts }
        switch TranscriptCover.select(
            answer: model.answerSheet,
            dock: false,
            thinking: thinkingOpen,
            context: contextOpen && model.contextSheet != nil,
            command: model.overlay != nil
        ) {
        case .answer(let sheet):
            return .answer(sheet)
        case .dock:
            return nil
        case .thinking:
            return .thinking
        case .context:
            guard let sheet = model.contextSheet else {
                return nil
            }
            return .context(sheet)
        case .command:
            guard let page = model.overlay else {
                return nil
            }
            return .command(page)
        case nil:
            return nil
        }
    }

    @ViewBuilder
    private func coverPage(_ item: ShownCover) -> some SwiftUI.View {
        switch item {
        case .image(let image):
            ImagePreviewView(attachment: image)
        case .artifacts:
            ArtifactsSheet(model: model, sessionId: sessionId)
                .id(sessionId)
        case .goal:
            goalPage
        case .answer(let sheet):
            AnswerSheetView(model: model, sheet: sheet)
        case .pasted(let text):
            ScrollView {
                Text(verbatim: text)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(16)
            }
            .accessibilityIdentifier("complete-pasted-input")
        case .text(let card):
            ScrollView {
                CardBlock(card: card, onOpenCommands: {}, onOpenPermission: { _ in }, onOpenText: { _ in }, onOpenImage: model.previewImage, onEnhance: { _, _ in }, expanded: true)
                    .textSelection(.enabled)
                    .padding(.horizontal, 20)
                    .padding(.vertical, 28)
            }
            .accessibilityIdentifier("complete-card-text")
        case .thinking:
            ThinkingSheet(model: model)
        case .context(let sheet):
            ContextSheetView(sheet: sheet)
        case .command(let page):
            switch page {
            case .palette:
                PaletteSheet(model: model)
            case .model:
                ModelSheet(model: model)
            case .effort:
                EffortSheet(model: model)
            case .profile:
                ProfileSheet(model: model)
            }
        }
    }

    private func releaseCover() {
        if model.shownImage != nil {
            model.closeImage()
            return
        }
        if pastedText != nil {
            pastedText = nil
            return
        }
        if expandedCard != nil {
            expandedCard = nil
            return
        }
        if artifactsOpen {
            artifactsOpen = false
            return
        }
        if goalOpen {
            goalOpen = false
            return
        }
        switch TranscriptCover.select(
            answer: model.answerSheet,
            dock: false,
            thinking: thinkingOpen,
            context: contextOpen && model.contextSheet != nil,
            command: model.overlay != nil
        ) {
        case .answer(let sheet):
            if let eventId = AnswerSheet.permissionDismissed(byNil: sheet) {
                model.dismissPermission(eventId)
            } else if AnswerSheet.questionDismissed(byNil: sheet) != nil {
                model.dismissQuestion()
            }
        case .dock:
            break
        case .thinking:
            thinkingOpen = false
        case .context:
            contextOpen = false
        case .command:
            model.dismissOverlay()
        case nil:
            break
        }
    }

    private var sendDisabled: Bool {
        model.sending || (model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && model.images.isEmpty)
    }

}

private enum ShownCover: Identifiable {
    case artifacts
    case goal
    case answer(AnswerSheet)
    case image(ImageAttachment)
    case text(Card)
    case pasted(String)
    case thinking
    case context(ContextSheet)
    case command(PhoneOverlay)

    var id: String {
        switch self {
        case .image(let image):
            "image-" + image.id
        case .pasted:
            "pasted"
        case .text(let card):
            "text-" + card.id
        case .artifacts:
            "artifacts"
        case .goal:
            "goal"
        case .answer(let sheet):
            "answer-" + sheet.id
        case .thinking:
            "thinking"
        case .context:
            "context"
        case .command(let page):
            "command-" + page.id
        }
    }
}

struct TranscriptCards: SwiftUI.View {
    var cards: [Card]
    var revision: Int
    var onOpenCommands: () -> Void
    var onOpenPermission: (String) -> Void
    var onOpenText: (Card) -> Void
    var onOpenImage: (ImageAttachment) -> Void = { _ in }
    var onOpenArtifact: ((ArtifactFile) async throws -> Data)?
    var onEnhance: (String, String?) -> Void
    @State private var followTail = true

    var body: some SwiftUI.View {
        ScrollViewReader { proxy in
            GeometryReader { viewport in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 18) {
                        ForEach(cards, id: \.id) { card in
                            CardBlock(
                                card: card,
                                onOpenCommands: onOpenCommands,
                                onOpenPermission: onOpenPermission,
                                onOpenText: onOpenText,
                                onOpenImage: onOpenImage,
                                onOpenArtifact: onOpenArtifact,
                                onEnhance: onEnhance
                            )
                                .id(card.id)
                        }
                        Color.clear
                            .frame(height: 1)
                            .id(TranscriptCards.tail)
                    }
                    .padding(.horizontal)
                    .padding(.vertical, 12)
                    .frame(maxWidth: .infinity, minHeight: viewport.size.height, alignment: .topLeading)
                    .background {
                        Color.clear
                            .contentShape(Rectangle())
                            .transcriptTouch(pointer: .cardList, onOpenCommands: onOpenCommands)
                    }
                }
                .defaultScrollAnchor(.bottom)
                .defaultScrollAnchor(.top, for: .alignment)
                .onScrollGeometryChange(for: TranscriptScrollPosition.self) { geometry in
                    TranscriptScrollPosition(
                        atBottom: geometry.visibleRect.maxY >= geometry.contentSize.height - 24,
                        viewportHeight: geometry.visibleRect.height
                    )
                } action: { previous, current in
                    if previous.atBottom && previous.viewportHeight != current.viewportHeight {
                        proxy.scrollTo(TranscriptCards.tail, anchor: .bottom)
                    }
                    followTail = current.atBottom
                }
                .scrollDismissesKeyboard(.interactively)
                .onChange(of: revision) { _, _ in
                    guard followTail else { return }
                    proxy.scrollTo(TranscriptCards.tail, anchor: .bottom)
                }
            }
        }
    }

    static let tail = "tail"
}

private struct TranscriptScrollPosition: Equatable {
    var atBottom: Bool
    var viewportHeight: CGFloat
}

struct CardBlock: SwiftUI.View {
    var card: Card
    var onOpenCommands: () -> Void
    var onOpenPermission: (String) -> Void
    var onOpenText: (Card) -> Void
    var onOpenImage: (ImageAttachment) -> Void = { _ in }
    var onOpenArtifact: ((ArtifactFile) async throws -> Data)?
    var onEnhance: (String, String?) -> Void
    var expanded = false

    var body: some SwiftUI.View {
        if expanded {
            VStack(alignment: .leading, spacing: 16) {
                cardBody
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            let pointer = transcriptPointerForCardTap(holdsLink: cardHoldsLink(card.body))
            aligned.transcriptTouch(pointer: pointer, onOpenCommands: onOpenCommands) {
                if case .permission(let permission) = card.body, permission.decision == nil {
                    onOpenPermission(permission.eventId)
                }
            }
        }
    }

    private var aligned: some SwiftUI.View {
        let isUser = cardEdge(card.kind) == .trailing
        return HStack(alignment: .top, spacing: 0) {
            if isUser {
                Spacer(minLength: 40)
            }
            VStack(alignment: .leading, spacing: 8) {
                if !isUser {
                    Text(agentLabel)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Ink.faint)
                }
                cardBody
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .background(
                isUser ? Ink.accent.opacity(0.12) : Ink.card,
                in: RoundedRectangle(cornerRadius: 20)
            )
            .frame(maxWidth: 600, alignment: isUser ? .trailing : .leading)
            if !isUser {
                Spacer(minLength: 40)
            }
        }
        .frame(maxWidth: .infinity, alignment: isUser ? .trailing : .leading)
        .accessibilityIdentifier("transcript-card-" + card.id)
    }

    private var agentLabel: String {
        switch card.kind {
        case .ask, .answer, .result: "Kyoto"
        case .question: "Kyoto · Question"
        case .permission: "Kyoto · Permission"
        case .proof: "Kyoto · Verification"
        case .artifact: "Kyoto · File"
        case .enhance: "Kyoto · Suggested prompt"
        }
    }

    @ViewBuilder
    private var cardBody: some SwiftUI.View {
        if case .artifact(let artifact) = card.body {
            ArtifactCardView(file: artifact.file, caption: artifact.caption, download: onOpenArtifact)
        } else {
            let parts = transcriptCardText(expanded ? card.body : cardTextPreview(card.body))
            ForEach(Array(parts.enumerated()), id: \.offset) { _, part in
                switch part {
                case .markdown(let blocks):
                    MarkdownBlocksView(blocks: blocks)
                case .plain(let text, let role):
                    plain(text, role)
                }
            }
        }
        if !expanded && cardHasTextPreview(card.body) {
            Button("Show complete text") { onOpenText(card) }
                .buttonStyle(.plain)
                .foregroundStyle(Ink.accent)
                .accessibilityIdentifier("show-complete-text-" + card.id)
        }
        if case .ask(let ask) = card.body, let images = ask.images {
            ImageAttachmentStrip(images: images, onOpen: onOpenImage)
        }
        if case .enhance(let enhance) = card.body {
            EnhanceCardActions(card: enhance, onPost: onEnhance)
        }
    }

    @ViewBuilder
    private func plain(_ text: String, _ role: TranscriptPlainRole) -> some SwiftUI.View {
        let line = Text(AttributedString(text))
        switch role {
        case .permissionAction:
            line.foregroundStyle(Ink.text)
        case .body, .choices:
            line.font(.subheadline).foregroundStyle(role == .choices ? Ink.faint : Ink.text)
        case .permissionPath, .note, .proofItem:
            line.font(.caption).foregroundStyle(Ink.faint)
        case .permissionDiff:
            line.font(.caption.monospaced()).foregroundStyle(Ink.faint)
        }
    }
}

private extension SwiftUI.View {
    @ViewBuilder
    func transcriptTouch(
        pointer: TranscriptPointer,
        onOpenCommands: @escaping () -> Void,
        onTap: @escaping () -> Void = {}
    ) -> some SwiftUI.View {
        let opened = highPriorityGesture(
            TapGesture(count: 2).onEnded { _ in
                if transcriptDoubleTap(pointer) == .openPalette {
                    onOpenCommands()
                }
            }
        )
        if transcriptRequestsKeyboardDismissal(pointer) {
            opened.onTapGesture {
                ComposerKeyboard.dismiss()
                onTap()
            }
        } else {
            opened.onTapGesture(perform: onTap)
        }
    }
}

private struct EnhanceCardActions: SwiftUI.View {
    var card: EnhanceCard
    var onPost: (String, String?) -> Void
    @State private var editing = false
    @State private var draft = ""

    var body: some SwiftUI.View {
        VStack(alignment: .leading, spacing: 8) {
            if editing {
                TextField("Prompt", text: $draft, axis: .vertical)
                    .lineLimit(2...8)
                    .foregroundStyle(Ink.text)
                    .accessibilityIdentifier("enhance-draft")
                Button("Edit") {
                    let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !text.isEmpty else {
                        return
                    }
                    let posted = enhanceAnswer(.edit(text))
                    onPost(posted.choice, posted.text)
                }
                .buttonStyle(.plain)
                .foregroundStyle(Ink.accent)
                .accessibilityIdentifier("enhance-revise")
            } else {
                HStack(spacing: 12) {
                    choice("Use", .use, "enhance-use")
                    Button("Edit") {
                        draft = card.text
                        editing = true
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Ink.text)
                    .accessibilityIdentifier("enhance-edit")
                    choice("Discard", .discard, "enhance-discard")
                }
            }
        }
    }

    private func choice(_ title: String, _ post: EnhancePost, _ identifier: String) -> some SwiftUI.View {
        Button(title) {
            let posted = enhanceAnswer(post)
            onPost(posted.choice, posted.text)
        }
        .buttonStyle(.plain)
        .foregroundStyle(title == "Discard" ? Ink.bad : Ink.accent)
        .accessibilityIdentifier(identifier)
    }
}

enum ComposerKeyboard {
    static func dismiss() {
        #if canImport(UIKit)
        UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
        #endif
    }
}

struct MarkdownBlocksView: SwiftUI.View {
    var blocks: [TranscriptBlock]

    var body: some SwiftUI.View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                blockText(block)
            }
        }
    }

    @ViewBuilder
    private func blockText(_ block: TranscriptBlock) -> some SwiftUI.View {
        switch block {
        case .paragraph(let runs):
            runLine(runs, heading: nil)
                .lineSpacing(3)
                .fixedSize(horizontal: false, vertical: true)
        case .heading(let level, let runs):
            runLine(runs, heading: level)
                .padding(.top, 8)
                .fixedSize(horizontal: false, vertical: true)
        case .bullet(let runs):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("•")
                runLine(runs, heading: nil)
                    .fixedSize(horizontal: false, vertical: true)
            }
        case .numbered(let number, let runs):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("\(number).")
                    .monospacedDigit()
                runLine(runs, heading: nil)
                    .fixedSize(horizontal: false, vertical: true)
            }
        case .quote(let runs):
            runLine(runs, heading: nil)
                .foregroundStyle(.secondary)
                .padding(.leading, 12)
                .overlay(alignment: .leading) {
                    Rectangle().fill(Ink.line).frame(width: 2)
                }
        case .code(let source):
            ScrollView(.horizontal) {
                Text(verbatim: source)
                    .font(.callout.monospaced())
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(12)
            }
            .background(Ink.card, in: RoundedRectangle(cornerRadius: 8))
        case .table(let rows):
            ScrollView(.horizontal) {
                Grid(alignment: .leading, horizontalSpacing: 20, verticalSpacing: 10) {
                    ForEach(Array(rows.enumerated()), id: \.offset) { rowIndex, cells in
                        GridRow {
                            ForEach(Array(cells.enumerated()), id: \.offset) { _, runs in
                                runLine(runs, heading: rowIndex == 0 ? 3 : nil)
                                    .fixedSize(horizontal: true, vertical: false)
                            }
                        }
                        if rowIndex == 0 {
                            Divider().gridCellUnsizedAxes(.horizontal)
                        }
                    }
                }
                .padding(.vertical, 8)
            }
        }
    }

    private func runLine(_ runs: [TranscriptRun], heading: Int?) -> Text {
        runs.reduce(Text(verbatim: "")) { partial, run in
            partial + styled(run, heading: heading)
        }
    }

    private func styled(_ run: TranscriptRun, heading: Int?) -> Text {
        var attributed = AttributedString(run.text)
        attributed.foregroundColor = Ink.text
        var intent = InlinePresentationIntent()
        if run.bold || heading != nil {
            intent.insert(.stronglyEmphasized)
        }
        if run.italic {
            intent.insert(.emphasized)
        }
        if run.strike {
            intent.insert(.strikethrough)
        }
        if run.code {
            intent.insert(.code)
        }
        if !intent.isEmpty {
            attributed.inlinePresentationIntent = intent
        }
        if let link = run.link, let url = URL(string: link) {
            attributed.link = url
        }
        if let heading, !run.code {
            switch heading {
            case 1:
                attributed.font = .title2
            case 2:
                attributed.font = .title3
            default:
                attributed.font = .headline
            }
        }
        return Text(attributed)
    }

}
