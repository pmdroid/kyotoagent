import XCTest

@MainActor
final class ComposerTests: XCTestCase {
    func testCommandCompletionAndNativeCursorEditing() throws {
        continueAfterFailure = false
        let pairing = try XCTUnwrap(ProcessInfo.processInfo.environment["COMPOSER_PAIRING_URL"])
        let url = try XCTUnwrap(URL(string: pairing))
        let app = XCUIApplication()
        app.launch()
        app.open(url)
        let session = app.buttons.containing(.staticText, identifier: "Composer UI test").firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 30), app.debugDescription)
        session.tap()
        let field = app.descendants(matching: .any)["composer-draft"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.tap()
        typeAtKeyboardPace("/mod", into: field)
        let command = app.buttons["slash-match-model"]
        XCTAssertTrue(command.waitForExistence(timeout: 5))
        capture("Command autocomplete")
        command.tap()
        XCTAssertEqual(field.value as? String, "/model ")
        typeAtKeyboardPace("fixture-model", into: field)
        XCTAssertEqual(field.value as? String, "/model fixture-model")
        app.buttons["composer-send"].tap()
        XCTAssertTrue(app.staticTexts["Model saved"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.staticTexts["composer-notice"].exists)
        field.tap()
        typeAtKeyboardPace("hello world", into: field)
        field.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0.5))
            .withOffset(CGVector(dx: 45, dy: 0)).tap()
        typeAtKeyboardPace("ZZ", into: field)
        let edited = field.value as? String ?? ""
        XCTAssertEqual(edited.replacingOccurrences(of: "ZZ", with: ""), "hello world")
        XCTAssertTrue(edited.contains("ZZ"))
        XCTAssertFalse(edited.hasSuffix("ZZ"))
        capture("Native tap to edit")
        app.buttons["composer-send"].tap()
        XCTAssertTrue(app.staticTexts[edited].waitForExistence(timeout: 10))
        XCTAssertFalse(app.staticTexts["composer-notice"].exists)
        field.tap()
        typeAtKeyboardPace("/pre", into: field)
        let skill = app.buttons["slash-match-preflight"]
        XCTAssertTrue(skill.waitForExistence(timeout: 5))
        skill.tap()
        XCTAssertEqual(field.value as? String, "/preflight ")
        capture("Skill autocomplete")
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: "/preflight ".count))
        verifyCloseout(in: app, field: field)
    }

    private func verifyCloseout(in app: XCUIApplication, field: XCUIElement) {
        field.tap()
        typeAtKeyboardPace("/fixture closeout", into: field)
        app.buttons["composer-send"].tap()
        let accept = app.buttons["answer-choice-Accept failed closeout for this session"]
        XCTAssertTrue(accept.waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertTrue(app.buttons["answer-choice-Stop"].exists)
        XCTAssertTrue(accept.isHittable)
        XCTAssertLessThan(accept.frame.maxY, app.windows.firstMatch.frame.maxY - 44)
        capture("Closeout retry limit")
        accept.tap()
        let commands = app.buttons["chat-commands"]
        XCTAssertTrue(commands.waitForExistence(timeout: 10))
        commands.tap()
        let closeout = app.buttons["palette-closeout"]
        XCTAssertTrue(closeout.waitForExistence(timeout: 10))
        closeout.tap()
        let sheet = app.otherElements.matching(identifier: "phone-popup")
            .containing(.navigationBar, identifier: "Closeout").firstMatch
        XCTAssertTrue(sheet.waitForExistence(timeout: 10))
        XCTAssertTrue(sheet.staticTexts["closeout-accepted"].exists)
        XCTAssertTrue(sheet.staticTexts["Test suite failed"].exists)
        capture("Accepted closeout with retained failure")
        XCTAssertGreaterThanOrEqual(sheet.buttons["closeout-enable"].frame.height, 44)
        sheet.buttons["closeout-enable"].tap()
        XCTAssertTrue(sheet.staticTexts["closeout-required"].waitForExistence(timeout: 10))
        XCTAssertFalse(sheet.staticTexts["closeout-accepted"].exists)
        XCTAssertTrue(sheet.staticTexts["Test suite failed"].exists)
        capture("Closeout enabled again")
    }

    private func typeAtKeyboardPace(_ text: String, into field: XCUIElement) {
        for character in text {
            field.typeText(String(character))
        }
    }

    private func capture(_ name: String) {
        let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
