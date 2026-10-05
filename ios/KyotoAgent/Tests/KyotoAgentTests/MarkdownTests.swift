import XCTest
@testable import KyotoAgent

final class MarkdownTests: XCTestCase {
    func testAResultFormatsBoldListAndCode() throws {
        let parts = transcriptCardText(.result(ResultCard(
            text: "**bold**\n\n- item\n\n`code`\n\n**`code`**",
            note: nil
        )))
        XCTAssertEqual(parts, [
            .markdown([
                .paragraph([TranscriptRun(text: "bold", bold: true)]),
                .bullet([TranscriptRun(text: "item")]),
                .paragraph([TranscriptRun(text: "code", code: true)]),
                .paragraph([TranscriptRun(text: "code", bold: true, code: true)]),
            ])
        ])
        let transcript = try String(contentsOf: packageRoot().appendingPathComponent("App/TranscriptScreen.swift"), encoding: .utf8)
        XCTAssertTrue(transcript.contains("transcriptCardText(expanded ? card.body : cardTextPreview(card.body))"))
        XCTAssertTrue(transcript.contains("inlinePresentationIntent"))
        XCTAssertTrue(transcript.contains(".stronglyEmphasized"))
        XCTAssertTrue(transcript.contains(".emphasized"))
        XCTAssertTrue(transcript.contains(".strikethrough"))
        XCTAssertTrue(transcript.contains(".code"))
        XCTAssertTrue(transcript.contains("foregroundColor"))
        XCTAssertTrue(transcript.contains("attributed.link"))
        XCTAssertTrue(transcript.contains(".permissionPath"))
        XCTAssertFalse(transcript.contains("AttributedString(markdown:"))
        XCTAssertFalse(transcript.contains("try? AttributedString"))
    }

    func testABrokenSpanLeavesTheRestFormatted() {
        let blocks = transcriptBlocks("**bold** and `unclosed <T> *italic*\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n**after**")
        XCTAssertEqual(blocks, [
            .paragraph([
                TranscriptRun(text: "bold", bold: true),
                TranscriptRun(text: " and `unclosed <T> "),
                TranscriptRun(text: "italic", italic: true),
            ]),
            .table([
                [[TranscriptRun(text: "a")], [TranscriptRun(text: "b")]],
                [[TranscriptRun(text: "1")], [TranscriptRun(text: "2")]],
            ]),
            .paragraph([TranscriptRun(text: "after", bold: true)]),
        ])
    }

    func testTableCellsKeepInlineFormattingAndEscapedPipes() {
        XCTAssertEqual(transcriptBlocks("| **Name** | Value |\n| --- | --- |\n| a\\|b | `code` |"), [
            .table([
                [[TranscriptRun(text: "Name", bold: true)], [TranscriptRun(text: "Value")]],
                [[TranscriptRun(text: "a|b")], [TranscriptRun(text: "code", code: true)]],
            ])
        ])
    }

    func testAnAnswerAndAQuestionPromptUseTheSameFormatter() {
        let source = "**same**"
        let formatted = transcriptBlocks(source)
        let answer = transcriptCardText(.answer(TextCard(text: source)))
        let question = transcriptCardText(.question(QuestionCard(
            answer: "typed",
            choices: ["Keep"],
            eventId: "q1",
            text: source
        )))
        let ask = transcriptCardText(.ask(TextCard(text: source)))
        let proof = transcriptCardText(.proof(ProofCard(
            text: source,
            items: [ProofItem(id: "cargo-test", kind: "command", outcome: "passed", argv: nil, exit: 0, tail: nil)]
        )))
        XCTAssertEqual(answer, [.markdown(formatted)])
        XCTAssertEqual(question, [
            .markdown(formatted),
            .plain("Keep", .choices),
            .plain("typed", .body),
        ])
        XCTAssertEqual(ask, [.markdown(formatted)])
        XCTAssertEqual(proof, [
            .markdown(formatted),
            .plain("cargo-test · passed", .proofItem),
        ])
        XCTAssertEqual(formatted, [.paragraph([TranscriptRun(text: "same", bold: true)])])
    }

    func testAPermissionPathStaysPlain() {
        let path = "/tmp/**bold** and `code`"
        let parts = transcriptCardText(.permission(PermissionCard(
            action: "read **file**",
            argv: nil,
            decision: nil,
            diff: ["+ **added**"],
            eventId: "e1",
            path: path,
            timeoutSec: nil
        )))
        XCTAssertEqual(parts, [
            .plain("read **file**", .permissionAction),
            .plain(path, .permissionPath),
            .plain("+ **added**", .permissionDiff),
        ])
    }

    func testHeadingsQuotesFencesStrikesAndLinksStayFormatted() {
        let blocks = transcriptBlocks("# Title\n\n> quoted **bold**\n\n```\nlet x = 1\n```\n\n~~gone~~ and *lean* and [docs](https://example.com/a)")
        XCTAssertEqual(blocks, [
            .heading(1, [TranscriptRun(text: "Title")]),
            .quote([
                TranscriptRun(text: "quoted "),
                TranscriptRun(text: "bold", bold: true),
            ]),
            .code("let x = 1"),
            .paragraph([
                TranscriptRun(text: "gone", strike: true),
                TranscriptRun(text: " and "),
                TranscriptRun(text: "lean", italic: true),
                TranscriptRun(text: " and "),
                TranscriptRun(text: "docs", link: "https://example.com/a"),
            ]),
        ])
    }
}

private func packageRoot() -> URL {
    URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
}

extension MarkdownTests {
    func testCompleteTextButtonAppearsAsSoonAsTextExceedsThePreview() {
        for text in [String(repeating: "a", count: 200), "one\ntwo\nthree"] {
            let body = CardBody.result(ResultCard(text: text, note: nil))
            XCTAssertFalse(cardHasTextPreview(body))
            XCTAssertEqual(cardTextPreview(body), body)
        }
        for text in [String(repeating: "a", count: 201), "one\ntwo\nthree\nfour"] {
            let body = CardBody.result(ResultCard(text: text, note: nil))
            XCTAssertTrue(cardHasTextPreview(body))
            XCTAssertNotEqual(cardTextPreview(body), body)
        }
    }

    func testLargeTextPreviewPreservesQuestionChoicesAndOriginal() {
        let text = (0..<80).map { "line \($0)" }.joined(separator: "\n")
        let question = QuestionCard(answer: nil, choices: ["continue"], eventId: "q", text: text)
        let original = CardBody.question(question)
        guard case .question(let preview) = cardTextPreview(original) else { XCTFail("question preview"); return }
        XCTAssertTrue(preview.text.contains("line 2"))
        XCTAssertFalse(preview.text.contains("line 3"))
        XCTAssertFalse(preview.text.contains("line 79"))
        XCTAssertEqual(preview.choices, question.choices)
        XCTAssertEqual(question.text, text)
        XCTAssertTrue(cardHasTextPreview(original))
        XCTAssertFalse(cardHasTextPreview(.result(ResultCard(text: "short", note: nil))))
        let unicode = String(repeating: "🐕", count: 3000)
        guard case .result(let preview) = cardTextPreview(.result(ResultCard(text: unicode, note: "kept"))) else { XCTFail("result preview"); return }
        XCTAssertEqual(preview.text.count, 200)
        XCTAssertEqual(preview.note, "kept")
    }
}
