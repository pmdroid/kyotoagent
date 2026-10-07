---
name: kyoto-plan
description: Plan a project or consequential change with concise decisions and visual questions in Kyoto's terminal and iOS app. Use when the user wants a plan, architecture choices, or a visual explanation before implementation.
---

# Plan together

Inspect the workspace and settle factual questions yourself. State the destination and how success will be demonstrated in two sentences. Keep a short todo list with named decisions, research, and work; attach detail through descriptions, files, and links.

Ask one consequential decision at a time with `ask`. Supply two to four short `choices`, including a custom-answer invitation when useful. Keep the question and recommendation below 60 words. Explain the recommendation in one sentence. Never put a waiting question in `finish`. Ask at most two independent questions in a round, never a whole questionnaire.

Use `visuals` when a picture replaces explanation:

- Mermaid for architecture, dependencies, flows, and sequence diagrams.
- A local SVG or image for a wireframe, screenshot, or visual alternative.
- At most two focused visuals, with short titles and plain-language `alt` text explaining the important relationship or difference.
- Supply either `mermaid` source or `path` for each visual. The server creates a safe image preview. Use PNG, JPEG, WebP, SVG, or a Mermaid text file.
- Keep diagrams small, with readable labels. For alternatives, title them to match the choice labels. Label proposed behavior as proposed and screenshots as observed only when they were actually captured.

For example:

```json
{
  "text": "When should review happen? I recommend before the PR so fixes are reviewed before publishing.",
  "choices": ["Before PR", "After draft PR", "Something else"],
  "visuals": [
    {
      "title": "A · Before PR",
      "alt": "Implement → review → fixes → review again → PR when clean.",
      "mermaid": "flowchart LR\n A[Implement] --> B[Review]\n B -->|Findings| C[Fix]\n C --> B\n B -->|Clean| D[PR]"
    },
    {
      "title": "B · After draft PR",
      "alt": "Implement → draft PR → review → fixes → review again.",
      "mermaid": "flowchart LR\n A[Implement] --> B[Draft PR]\n B --> C[Review]\n C -->|Findings| D[Fix]\n D --> C"
    }
  ]
}
```

After the answer, record the decision once and show only what changed. Continue within the authorized planning task without repeatedly requesting “go.” Ask again for genuinely new decisions or authority. Never treat silence as approval or an illustrative diagram as execution evidence.

Finish with a concise build sequence, acceptance checks, and the decision needed to start. Planning approval is not execution permission. Preserve the existing permission and closeout requirements.
