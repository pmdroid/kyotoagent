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
        field.typeText("/mod")
        let command = app.buttons["slash-match-model"]
        XCTAssertTrue(command.waitForExistence(timeout: 5))
        capture("Command autocomplete")
        command.tap()
        XCTAssertEqual(field.value as? String, "/model ")
        field.typeText("fixture-model")
        XCTAssertEqual(field.value as? String, "/model fixture-model")
        app.buttons["composer-send"].tap()
        XCTAssertTrue(app.staticTexts["Model saved"].waitForExistence(timeout: 10))
        field.tap()
        field.typeText("hello world")
        field.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0.5))
            .withOffset(CGVector(dx: 45, dy: 0)).tap()
        field.typeText("ZZ")
        let edited = field.value as? String ?? ""
        XCTAssertEqual(edited.replacingOccurrences(of: "ZZ", with: ""), "hello world")
        XCTAssertTrue(edited.contains("ZZ"))
        XCTAssertFalse(edited.hasSuffix("ZZ"))
        capture("Native tap to edit")
        app.buttons["composer-send"].tap()
        XCTAssertTrue(app.staticTexts[edited].waitForExistence(timeout: 10))
        field.tap()
        field.typeText("/pre")
        let skill = app.buttons["slash-match-preflight"]
        XCTAssertTrue(skill.waitForExistence(timeout: 5))
        skill.tap()
        XCTAssertEqual(field.value as? String, "/preflight ")
        capture("Skill autocomplete")
    }

    private func capture(_ name: String) {
        let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
