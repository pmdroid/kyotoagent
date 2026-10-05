import SwiftUI

struct DeleteConfirmCard: SwiftUI.View {
    @Bindable var model: AppModel
    @State private var deleteWorkspace = false

    var body: some SwiftUI.View {
        ZStack {
            Rectangle()
                .fill(.ultraThinMaterial)
                .ignoresSafeArea()
            if let confirm = model.deleteConfirm {
                VStack(alignment: .leading, spacing: 14) {
                    HStack {
                        Spacer()
                        Button {
                            model.cancelDelete()
                        } label: {
                            Image(systemName: "xmark")
                                .font(.body.weight(.semibold))
                                .foregroundStyle(Ink.faint)
                                .frame(width: 32, height: 32)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("delete-close")
                    }
                    ForEach(deleteConfirmLines(removesDirectory: deleteWorkspace && confirm.removesDirectory, workspace: confirm.workspace), id: \.self) { line in
                        Text(line)
                            .font(line == deletePrompt ? .headline : .body)
                            .foregroundStyle(line == deletePrompt ? Ink.text : Ink.faint)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if confirm.removesDirectory {
                        Button {
                            deleteWorkspace.toggle()
                        } label: {
                            Label("Also delete worktree", systemImage: deleteWorkspace ? "checkmark.square.fill" : "square")
                                .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                        }
                        .buttonStyle(.plain)
                        .accessibilityValue(deleteWorkspace ? "Checked" : "Unchecked")
                        .accessibilityIdentifier("delete-worktree-checkbox")
                    }
                    Button {
                        Swift.Task { await model.confirmDelete(deleteWorkspace: deleteWorkspace) }
                    } label: {
                        Text("Delete")
                            .font(.body.weight(.semibold))
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 10)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Ink.bad)
                    .accessibilityIdentifier("delete-confirm")
                    Button("Cancel") {
                        model.cancelDelete()
                    }
                    .buttonStyle(.bordered)
                    .frame(maxWidth: .infinity)
                    .accessibilityIdentifier("delete-cancel")
                }
                .padding(20)
                .frame(maxWidth: 360)
                .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 18, style: .continuous)
                        .stroke(Ink.line, lineWidth: 1)
                )
                .padding(28)
            }
        }
        .onChange(of: model.deleteConfirm?.id) { _, _ in deleteWorkspace = false }
        .accessibilityIdentifier("delete-card")
    }
}
