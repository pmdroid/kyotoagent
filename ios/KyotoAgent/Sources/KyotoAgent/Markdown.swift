import Foundation

nonisolated struct TranscriptRun: Equatable, Sendable {
    var text: String
    var bold: Bool
    var italic: Bool
    var strike: Bool
    var code: Bool
    var link: String?

    init(
        text: String,
        bold: Bool = false,
        italic: Bool = false,
        strike: Bool = false,
        code: Bool = false,
        link: String? = nil
    ) {
        self.text = text
        self.bold = bold
        self.italic = italic
        self.strike = strike
        self.code = code
        self.link = link
    }
}

nonisolated enum TranscriptBlock: Equatable, Sendable {
    case paragraph([TranscriptRun])
    case heading(Int, [TranscriptRun])
    case bullet([TranscriptRun])
    case numbered(Int, [TranscriptRun])
    case quote([TranscriptRun])
    case code(String)
    case table([[[TranscriptRun]]])
}

nonisolated enum TranscriptPlainRole: Equatable, Sendable {
    case body
    case choices
    case note
    case permissionAction
    case permissionPath
    case permissionDiff
    case proofItem
}

nonisolated enum TranscriptPart: Equatable, Sendable {
    case markdown([TranscriptBlock])
    case plain(String, TranscriptPlainRole)
}

nonisolated func transcriptBlocks(_ source: String) -> [TranscriptBlock] {
    MarkdownParser(source: source).blocks()
}

nonisolated func transcriptCardText(_ body: CardBody) -> [TranscriptPart] {
    switch body {
    case .ask(let text):
        return [.markdown(transcriptBlocks(text.text))]
    case .answer(let text):
        return [.markdown(transcriptBlocks(text.text))]
    case .question(let question):
        var parts: [TranscriptPart] = [.markdown(transcriptBlocks(question.text))]
        if !question.choices.isEmpty {
            parts.append(.plain(question.choices.joined(separator: "\n"), .choices))
        }
        if let answer = question.answer, !answer.isEmpty {
            parts.append(.plain(answer, .body))
        }
        return parts
    case .permission(let permission):
        var parts: [TranscriptPart] = [
            .plain(permission.decision?.rawValue ?? permission.action, .permissionAction)
        ]
        if let path = permission.path, !path.isEmpty {
            parts.append(.plain(path, .permissionPath))
        }
        if let diff = permission.diff, !diff.isEmpty {
            parts.append(.plain(diff.joined(separator: "\n"), .permissionDiff))
        }
        return parts
    case .result(let result):
        var parts: [TranscriptPart] = [.markdown(transcriptBlocks(result.text))]
        if let note = result.note, !note.isEmpty {
            parts.append(.plain(note, .note))
        }
        return parts
    case .artifact(let artifact):
        return [.plain(artifact.caption ?? artifact.file.name, .body), .plain(artifact.file.mediaType + " · " + String(artifact.file.size) + " bytes", .note)]
    case .proof(let proof):
        var parts: [TranscriptPart] = [.markdown(transcriptBlocks(proof.text))]
        for item in proof.items {
            let outcome = item.outcome == "passed_earlier" ? "✓ (earlier)" : item.outcome
            parts.append(.plain(item.id + " · " + outcome, .proofItem))
        }
        return parts
    case .enhance(let enhance):
        var parts: [TranscriptPart] = [.markdown(transcriptBlocks(enhance.text))]
        if let error = enhance.error, !error.isEmpty {
            parts.append(.plain(error, .note))
        }
        return parts
    }
}

nonisolated private struct MarkdownParser {
    let lines: [String]

    init(source: String) {
        lines = source.split(separator: "\n", omittingEmptySubsequences: false).map { line in
            var text = String(line)
            if text.hasSuffix("\r") {
                text.removeLast()
            }
            return text
        }
    }

    func blocks() -> [TranscriptBlock] {
        var blocks: [TranscriptBlock] = []
        var index = 0
        while index < lines.count {
            if lines[index].trimmingCharacters(in: .whitespaces).isEmpty {
                index += 1
                continue
            }
            if let fence = openingFence(lines[index]), let end = closingFence(mark: fence.mark, count: fence.count, after: index) {
                let body = lines[(index + 1)..<end].joined(separator: "\n")
                if !body.isEmpty {
                    blocks.append(.code(body))
                }
                index = end + 1
                continue
            }
            if let end = tableEnd(at: index) {
                let rows = [lines[index]] + Array(lines[(index + 2)..<end])
                blocks.append(.table(rows.map(tableCells)))
                index = end
                continue
            }
            if let heading = heading(lines[index]) {
                blocks.append(.heading(heading.level, parseInline(heading.text)))
                index += 1
                continue
            }
            if quoteBody(lines[index]) != nil {
                var gathered: [String] = []
                while index < lines.count, let body = quoteBody(lines[index]) {
                    gathered.append(body)
                    index += 1
                }
                blocks.append(.quote(parseInline(gathered.joined(separator: "\n"))))
                continue
            }
            if let item = listItem(lines[index]) {
                var text = item.text
                index += 1
                while index < lines.count, continuesListItem(lines[index]) {
                    text += "\n" + lines[index].trimmingCharacters(in: .whitespaces)
                    index += 1
                }
                let runs = parseInline(text)
                if let number = item.number {
                    blocks.append(.numbered(number, runs))
                } else {
                    blocks.append(.bullet(runs))
                }
                continue
            }
            var paragraph: [String] = []
            while index < lines.count {
                let line = lines[index]
                if line.trimmingCharacters(in: .whitespaces).isEmpty {
                    break
                }
                if !paragraph.isEmpty && startsBlock(at: index) {
                    break
                }
                paragraph.append(line)
                index += 1
            }
            let runs = parseInline(paragraph.joined(separator: "\n"))
            if !runs.isEmpty {
                blocks.append(.paragraph(runs))
            }
        }
        return blocks
    }

    private func startsBlock(at index: Int) -> Bool {
        let line = lines[index]
        if openingFence(line) != nil || heading(line) != nil || quoteBody(line) != nil || listItem(line) != nil {
            return true
        }
        return tableEnd(at: index) != nil
    }

    private func openingFence(_ line: String) -> (mark: Character, count: Int)? {
        let body = line.drop(while: { $0 == " " })
        let indent = line.count - body.count
        guard indent < 4, let mark = body.first, mark == "`" || mark == "~" else {
            return nil
        }
        var count = 0
        for character in body {
            if character == mark {
                count += 1
            } else {
                break
            }
        }
        guard count >= 3 else {
            return nil
        }
        let rest = body.dropFirst(count)
        if mark == "`" && rest.contains("`") {
            return nil
        }
        return (mark, count)
    }

    private func closingFence(mark: Character, count: Int, after open: Int) -> Int? {
        var index = open + 1
        while index < lines.count {
            let body = lines[index].drop(while: { $0 == " " })
            let indent = lines[index].count - body.count
            if indent < 4 {
                var ticks = 0
                for character in body {
                    if character == mark {
                        ticks += 1
                    } else {
                        break
                    }
                }
                let rest = body.dropFirst(ticks)
                if ticks >= count && rest.allSatisfy({ $0 == " " || $0 == "\t" }) {
                    return index
                }
            }
            index += 1
        }
        return nil
    }

    private func tableCells(_ line: String) -> [[TranscriptRun]] {
        var text = line.trimmingCharacters(in: .whitespaces)
        if text.hasPrefix("|") { text.removeFirst() }
        if text.hasSuffix("|") { text.removeLast() }
        var cells = [""]
        var escaped = false
        for character in text {
            if character == "|" && !escaped {
                cells.append("")
            } else {
                cells[cells.count - 1].append(character)
            }
            escaped = character == "\\" && !escaped
        }
        return cells.map { parseInline($0.trimmingCharacters(in: .whitespaces)) }
    }

    private func tableEnd(at index: Int) -> Int? {
        guard index + 1 < lines.count else {
            return nil
        }
        let header = lines[index]
        guard header.contains("|"), isSeparatorRow(lines[index + 1]) else {
            return nil
        }
        var end = index + 2
        while end < lines.count {
            let line = lines[end]
            if line.trimmingCharacters(in: .whitespaces).isEmpty || !line.contains("|") {
                break
            }
            end += 1
        }
        return end
    }

    private func isSeparatorRow(_ line: String) -> Bool {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        guard trimmed.contains("|"), trimmed.contains("-") else {
            return false
        }
        var cells = trimmed
        if cells.hasPrefix("|") {
            cells.removeFirst()
        }
        if cells.hasSuffix("|") {
            cells.removeLast()
        }
        let parts = cells.split(separator: "|", omittingEmptySubsequences: false)
        guard !parts.isEmpty else {
            return false
        }
        return parts.allSatisfy { part in
            var cell = part.trimmingCharacters(in: .whitespaces)
            guard cell.count >= 3 else {
                return false
            }
            if cell.hasPrefix(":") {
                cell.removeFirst()
            }
            if cell.hasSuffix(":") {
                cell.removeLast()
            }
            return !cell.isEmpty && cell.allSatisfy { $0 == "-" }
        }
    }

    private func heading(_ line: String) -> (level: Int, text: String)? {
        let body = line.drop(while: { $0 == " " })
        let indent = line.count - body.count
        guard indent < 4 else {
            return nil
        }
        var level = 0
        for character in body {
            if character == "#" && level < 7 {
                level += 1
            } else {
                break
            }
        }
        guard (1...6).contains(level) else {
            return nil
        }
        let rest = body.dropFirst(level)
        guard rest.first == " " || rest.first == "\t" else {
            return nil
        }
        return (level, rest.drop(while: { $0 == " " || $0 == "\t" }).description)
    }

    private func quoteBody(_ line: String) -> String? {
        let body = line.drop(while: { $0 == " " })
        let indent = line.count - body.count
        guard indent < 4, body.first == ">" else {
            return nil
        }
        var rest = body.dropFirst()
        if rest.first == " " {
            rest = rest.dropFirst()
        }
        return String(rest)
    }

    private func listItem(_ line: String) -> (number: Int?, text: String)? {
        let body = line.drop(while: { $0 == " " })
        let indent = line.count - body.count
        guard indent < 4, let first = body.first else {
            return nil
        }
        if first == "-" || first == "*" || first == "+" {
            let rest = body.dropFirst()
            guard rest.first == " " || rest.first == "\t" else {
                return nil
            }
            return (nil, rest.drop(while: { $0 == " " || $0 == "\t" }).description)
        }
        var digits = 0
        for character in body {
            if character.isNumber && digits < 9 {
                digits += 1
            } else {
                break
            }
        }
        guard digits > 0 else {
            return nil
        }
        let after = body.dropFirst(digits)
        guard after.first == "." || after.first == ")" else {
            return nil
        }
        let rest = after.dropFirst()
        guard rest.first == " " || rest.first == "\t" else {
            return nil
        }
        let number = Int(body.prefix(digits))
        return (number, rest.drop(while: { $0 == " " || $0 == "\t" }).description)
    }

    private func continuesListItem(_ line: String) -> Bool {
        guard !line.trimmingCharacters(in: .whitespaces).isEmpty else {
            return false
        }
        guard listItem(line) == nil, heading(line) == nil, quoteBody(line) == nil, openingFence(line) == nil else {
            return false
        }
        return line.first == " " || line.first == "\t"
    }
}

nonisolated private func parseInline(_ source: String) -> [TranscriptRun] {
    let chars = Array(source)
    return merge(parseInline(chars, from: 0, to: chars.count))
}

nonisolated private func parseInline(_ chars: [Character], from start: Int, to end: Int) -> [TranscriptRun] {
    var runs: [TranscriptRun] = []
    var index = start
    while index < end {
        if chars[index] == "\\", index + 1 < end {
            runs.append(TranscriptRun(text: String(chars[index + 1])))
            index += 2
            continue
        }
        if let code = codeSpan(chars, from: index, to: end) {
            runs.append(code.run)
            index = code.end
            continue
        }
        if let link = linkSpan(chars, from: index, to: end) {
            runs.append(contentsOf: link.runs)
            index = link.end
            continue
        }
        if let strike = strikeSpan(chars, from: index, to: end) {
            runs.append(contentsOf: strike.runs)
            index = strike.end
            continue
        }
        if let emphasis = emphasisSpan(chars, from: index, to: end) {
            runs.append(contentsOf: emphasis.runs)
            index = emphasis.end
            continue
        }
        if let link = autolink(chars, from: index, to: end) {
            runs.append(TranscriptRun(text: link.url, link: link.url))
            index = link.end
            continue
        }
        let next = plainEnd(chars, from: index, to: end)
        if next == index {
            runs.append(TranscriptRun(text: String(chars[index])))
            index += 1
        } else {
            runs.append(TranscriptRun(text: String(chars[index..<next])))
            index = next
        }
    }
    return runs
}

nonisolated private func codeSpan(_ chars: [Character], from start: Int, to end: Int) -> (run: TranscriptRun, end: Int)? {
    guard chars[start] == "`" else {
        return nil
    }
    var count = 0
    while start + count < end, chars[start + count] == "`" {
        count += 1
    }
    var index = start + count
    while index < end {
        if chars[index] == "`" {
            var marks = 0
            while index + marks < end, chars[index + marks] == "`" {
                marks += 1
            }
            if marks == count {
                var inner = Array(chars[(start + count)..<index])
                if inner.first == " ", inner.last == " ", inner.contains(where: { $0 != " " }) {
                    inner = Array(inner.dropFirst().dropLast())
                }
                return (TranscriptRun(text: String(inner), code: true), index + marks)
            }
            index += marks
        } else {
            index += 1
        }
    }
    return nil
}

nonisolated private func linkSpan(_ chars: [Character], from start: Int, to end: Int) -> (runs: [TranscriptRun], end: Int)? {
    guard chars[start] == "[" else {
        return nil
    }
    var index = start + 1
    while index < end, chars[index] != "]" {
        if chars[index] == "[" {
            return nil
        }
        index += 1
    }
    guard index < end, chars[index] == "]" else {
        return nil
    }
    let labelEnd = index
    index += 1
    guard index < end, chars[index] == "(" else {
        return nil
    }
    index += 1
    let urlStart = index
    while index < end, chars[index] != ")", chars[index] != " ", chars[index] != "\n" {
        index += 1
    }
    guard index < end, chars[index] == ")", index > urlStart else {
        return nil
    }
    let url = String(chars[urlStart..<index])
    let runs = paint(parseInline(chars, from: start + 1, to: labelEnd), link: url)
    guard !runs.isEmpty else {
        return nil
    }
    return (runs, index + 1)
}

nonisolated private func strikeSpan(_ chars: [Character], from start: Int, to end: Int) -> (runs: [TranscriptRun], end: Int)? {
    guard start + 1 < end, chars[start] == "~", chars[start + 1] == "~" else {
        return nil
    }
    if start + 2 < end, chars[start + 2] == "~" {
        return nil
    }
    var index = start + 2
    while index + 1 < end {
        if chars[index] == "~", chars[index + 1] == "~", index + 2 >= end || chars[index + 2] != "~" {
            let runs = paint(parseInline(chars, from: start + 2, to: index), strike: true)
            guard !runs.isEmpty else {
                return nil
            }
            return (runs, index + 2)
        }
        index += 1
    }
    return nil
}

nonisolated private func emphasisSpan(_ chars: [Character], from start: Int, to end: Int) -> (runs: [TranscriptRun], end: Int)? {
    let mark = chars[start]
    guard mark == "*" || mark == "_" else {
        return nil
    }
    var count = 0
    while start + count < end, chars[start + count] == mark, count < 3 {
        count += 1
    }
    guard count > 0 else {
        return nil
    }
    if mark == "_", !underscoreOpens(chars, at: start) {
        return nil
    }
    guard opensEmphasis(chars, at: start, count: count, end: end) else {
        return nil
    }
    var search = start + count
    while search < end {
        guard let found = markerRun(chars, mark: mark, from: search, to: end) else {
            return nil
        }
        if found.count >= count {
            let close = found.start + (found.count - count)
            if close > start + count, closesEmphasis(chars, at: close, count: count),
               mark != "_" || underscoreCloses(chars, at: close, count: count, end: end) {
                let inner = parseInline(chars, from: start + count, to: close)
                guard !inner.isEmpty else {
                    return nil
                }
                let runs = paint(
                    inner,
                    bold: count >= 2,
                    italic: count == 1 || count == 3
                )
                return (runs, close + count)
            }
        }
        search = found.start + found.count
    }
    return nil
}

nonisolated private func markerRun(_ chars: [Character], mark: Character, from start: Int, to end: Int) -> (start: Int, count: Int)? {
    var index = start
    while index < end {
        if chars[index] == mark {
            var count = 0
            while index + count < end, chars[index + count] == mark {
                count += 1
            }
            return (index, count)
        }
        index += 1
    }
    return nil
}

nonisolated private func opensEmphasis(_ chars: [Character], at index: Int, count: Int, end: Int) -> Bool {
    let after = index + count
    guard after < end else {
        return false
    }
    return chars[after] != " " && chars[after] != "\t" && chars[after] != "\n"
}

nonisolated private func closesEmphasis(_ chars: [Character], at index: Int, count: Int) -> Bool {
    guard index > 0 else {
        return false
    }
    let before = chars[index - 1]
    return before != " " && before != "\t" && before != "\n"
}

nonisolated private func underscoreOpens(_ chars: [Character], at index: Int) -> Bool {
    guard index > 0 else {
        return true
    }
    return !chars[index - 1].isLetter && !chars[index - 1].isNumber
}

nonisolated private func underscoreCloses(_ chars: [Character], at index: Int, count: Int, end: Int) -> Bool {
    let after = index + count
    guard after < end else {
        return true
    }
    return !chars[after].isLetter && !chars[after].isNumber
}

nonisolated private func autolink(_ chars: [Character], from start: Int, to end: Int) -> (url: String, end: Int)? {
    if start > 0 {
        let previous = chars[start - 1]
        if previous.isLetter || previous.isNumber {
            return nil
        }
    }
    let rest = String(chars[start..<end])
    let prefix = rest.hasPrefix("https://") ? "https://" : rest.hasPrefix("http://") ? "http://" : nil
    guard let prefix else {
        return nil
    }
    var index = start + prefix.count
    while index < end {
        let character = chars[index]
        if character.isWhitespace || character == "<" || character == ">" || character == ")" {
            break
        }
        index += 1
    }
    while index > start + prefix.count, ".,;:!?".contains(chars[index - 1]) {
        index -= 1
    }
    guard index > start + prefix.count else {
        return nil
    }
    return (String(chars[start..<index]), index)
}

nonisolated private func plainEnd(_ chars: [Character], from start: Int, to end: Int) -> Int {
    var index = start
    while index < end {
        let character = chars[index]
        if character == "\\" || character == "`" || character == "[" || character == "*" || character == "_" || character == "~" {
            break
        }
        if (character == "h" || character == "H"), autolink(chars, from: index, to: end) != nil {
            break
        }
        index += 1
    }
    return index
}

nonisolated private func paint(
    _ runs: [TranscriptRun],
    bold: Bool = false,
    italic: Bool = false,
    strike: Bool = false,
    link: String? = nil
) -> [TranscriptRun] {
    runs.map { run in
        var run = run
        if bold {
            run.bold = true
        }
        if italic {
            run.italic = true
        }
        if strike {
            run.strike = true
        }
        if let link, run.link == nil {
            run.link = link
        }
        return run
    }
}

nonisolated private func merge(_ runs: [TranscriptRun]) -> [TranscriptRun] {
    var merged: [TranscriptRun] = []
    for run in runs where !run.text.isEmpty {
        if var last = merged.last,
           last.bold == run.bold,
           last.italic == run.italic,
           last.strike == run.strike,
           last.code == run.code,
           last.link == run.link {
            last.text += run.text
            merged[merged.count - 1] = last
        } else {
            merged.append(run)
        }
    }
    return merged
}
