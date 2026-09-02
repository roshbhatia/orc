# Provider migration workflow

This example records a realistic staged migration. Research feeds
implementation. A verifier gates the implementation and can return feedback.
The verified implementation reports to the orchestrator.

Run it from the repository that owns the work:

```bash
./run.sh
orc
```

The script changes only Orc state for that repository. It does not launch an
agent. Harness hooks can later bind live sessions to the generated nodes.
