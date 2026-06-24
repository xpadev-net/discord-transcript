#!/usr/bin/env python3
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
COVERED_WORKFLOWS = (
    ".github/workflows/ci.yml",
    ".github/workflows/docker.yml",
)
USES_RE = re.compile(r"^\s*uses:\s*(?P<value>[^#\s]+)")
COMMIT_SHA_RE = re.compile(r"[0-9a-fA-F]{40}")


def read_repo_file(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def workflow_uses_entries(path: str) -> list[tuple[int, str]]:
    entries: list[tuple[int, str]] = []
    for line_number, line in enumerate(read_repo_file(path).splitlines(), start=1):
        match = USES_RE.match(line)
        if match:
            entries.append((line_number, match.group("value").strip("\"'")))
    return entries


def is_local_action(ref: str) -> bool:
    return ref.startswith("./") or ref.startswith("../")


def is_sha_pinned_action(ref: str) -> bool:
    if is_local_action(ref):
        return True

    _, separator, revision = ref.rpartition("@")
    return bool(separator and COMMIT_SHA_RE.fullmatch(revision))


def assert_workflow_actions_are_sha_pinned() -> None:
    errors: list[str] = []
    seen_entries = 0

    for path in COVERED_WORKFLOWS:
        for line_number, action_ref in workflow_uses_entries(path):
            seen_entries += 1
            if not is_sha_pinned_action(action_ref):
                errors.append(
                    f"{path}:{line_number}: "
                    f"uses must be pinned to a commit SHA: {action_ref}"
                )

    assert seen_entries, "expected at least one workflow action to keep this policy live"

    if errors:
        raise SystemExit("\n".join(errors))


def main() -> None:
    assert_workflow_actions_are_sha_pinned()
    print("Workflow action pinning policy checks passed.")


if __name__ == "__main__":
    main()
