import SwiftUI

struct ArtifactsSheet: SwiftUI.View {
    var model: AppModel
    var sessionId: String
    @Environment(\.dynamicTypeSize) private var typeSize
    @State private var history: [ArtifactVersion] = []
    @State private var loading = true
    @State private var failure: String?

    var body: some SwiftUI.View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 24) {
                    if let failure {
                        Section {
                            Text(failure).foregroundStyle(Ink.bad)
                            Button("Try again") { Swift.Task { await load() } }
                        }
                    }
                    ForEach(history.reversed()) { version in
                        if !version.proof.files.isEmpty {
                            VStack(alignment: .leading, spacing: 12) {
                                Text("Version \(version.version)")
                                    .font(.headline)
                                    .accessibilityAddTraits(.isHeader)
                                LazyVGrid(columns: typeSize.isAccessibilitySize ? [GridItem(.flexible())] : [GridItem(.adaptive(minimum: 160), alignment: .top)], alignment: .leading, spacing: 12) {
                                    ForEach(version.proof.files, id: \.id) { file in
                                        ArtifactCardView(
                                            file: file,
                                            download: { try await model.artifact($0, session: sessionId) }
                                        )
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                        .padding(12)
                                        .background(.background, in: RoundedRectangle(cornerRadius: 12))
                                    }
                                }
                            }
                        }
                    }
                }
                .padding()
            }
            .background(Color(uiColor: .systemGroupedBackground))
            .overlay {
                if loading && history.isEmpty {
                    ProgressView("Loading artifacts…")
                } else if failure == nil && history.allSatisfy({ $0.proof.files.isEmpty }) {
                    ContentUnavailableView("No artifacts yet", systemImage: "folder", description: Text("Files shared by the agent will appear here."))
                }
            }
            .navigationTitle("Artifacts")
            .navigationBarTitleDisplayMode(.inline)
            .refreshable { await load() }
            .task(id: sessionId) { await load() }
        }
    }

    private func load() async {
        loading = true
        failure = nil
        defer { loading = false }
        do {
            history = try await model.artifacts(session: sessionId)
        } catch is CancellationError {
        } catch {
            failure = (error as? HostError)?.serverMessage ?? error.localizedDescription
        }
    }
}
