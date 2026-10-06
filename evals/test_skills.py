import unittest

from skills import PASS_MARKER, score_trace


def call(tool, **arguments):
    return {"kind": "tool_call", "body": {"tool": tool, "args": arguments}}


def result(tool, output, failed=False):
    return {"kind": "tool_result", "body": {"tool": tool, "output": output, "is_error": failed}}


def loaded(name):
    return [call("use_skill", name=name), result("use_skill", f"<skill><name>{name}</name></skill>")]


class SkillScoringTests(unittest.TestCase):
    def test_failed_skill_load_is_missing(self):
        events = [call("use_skill", name="repair"), result("use_skill", "No skill found", True)]
        score = score_trace({"expected": {"repair": "work"}}, events)
        self.assertEqual(score["missing"], ["repair"])
        self.assertFalse(score["selection_pass"])

    def test_loading_after_a_read_is_late(self):
        events = [call("read_file", path="app.py"), *loaded("repair")]
        score = score_trace({"expected": {"repair": "work"}}, events)
        self.assertEqual(score["late"], ["repair"])

    def test_verification_skill_can_load_after_editing(self):
        events = [*loaded("repair"), call("search_replace", path="app.py"), *loaded("verify"), call("run", argv=["python3", "verify.py"]), result("run", f"exited 0\n{PASS_MARKER}")]
        score = score_trace({"expected": {"repair": "work", "verify": "verify"}}, events)
        self.assertTrue(score["selection_pass"])
        self.assertTrue(score["verification_pass"])

    def test_failed_unrelated_skill_attempt_counts_against_precision(self):
        events = [call("use_skill", name="disabled"), result("use_skill", "No skill found", True)]
        score = score_trace({"expected": {}}, events)
        self.assertEqual(score["unexpected"], ["disabled"])
        self.assertFalse(score["selection_pass"])

    def test_claimed_success_does_not_count_as_verification(self):
        events = [call("run", argv=["python3", "verify.py"]), result("run", "exited 1\nAssertionError", True), {"kind": "result", "body": {"text": "All tests passed"}}]
        score = score_trace({"expected": {}}, events)
        self.assertFalse(score["verification_pass"])


if __name__ == "__main__":
    unittest.main()
