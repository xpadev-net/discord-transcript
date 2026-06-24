#!/usr/bin/env python3
import re
import shlex
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_DIR = ROOT / ".github/workflows"
CI_WORKFLOW = ".github/workflows/ci.yml"
WORKFLOW_SUFFIXES = {".yaml", ".yml"}
USES_RE = re.compile(r"^\s*uses:\s*(?P<value>[^#\s]+)")
COMMIT_SHA_RE = re.compile(r"[0-9a-fA-F]{40}")
JOB_HEADER_RE = re.compile(r"^  (?P<name>[A-Za-z0-9_-]+):\s*$")
PATH_FILTER_RE = re.compile(r"^\s*paths(?:-ignore)?:\s*")


def read_repo_file(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def covered_workflows() -> list[str]:
    return sorted(
        str(path.relative_to(ROOT))
        for path in WORKFLOW_DIR.iterdir()
        if path.is_file() and path.suffix in WORKFLOW_SUFFIXES
    )


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
    workflows = covered_workflows()

    assert workflows, "expected at least one workflow file to keep this policy live"

    for path in workflows:
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


def workflow_mapping_block(contents: str, indent: int, key: str) -> tuple[int, str]:
    lines = contents.splitlines()
    prefix = f"{' ' * indent}{key}:"

    for index, line in enumerate(lines):
        if not line.startswith(prefix):
            continue

        for end_index in range(index + 1, len(lines)):
            candidate = lines[end_index]
            candidate_indent = len(candidate) - len(candidate.lstrip())
            if candidate.strip() and candidate_indent <= indent:
                return index + 1, "\n".join(lines[index:end_index])
        return index + 1, "\n".join(lines[index:])

    raise AssertionError(f"CI workflow must define {key}")


def workflow_job_block(contents: str, job_name: str) -> tuple[int, str]:
    lines = contents.splitlines()

    for index, line in enumerate(lines):
        if line == f"  {job_name}:":
            for end_index in range(index + 1, len(lines)):
                if JOB_HEADER_RE.match(lines[end_index]):
                    return index + 1, "\n".join(lines[index:end_index])
            return index + 1, "\n".join(lines[index:])

    raise AssertionError(f"CI workflow must define a {job_name} job")


def shell_continuation_command(lines: list[str], index: int, marker_offset: int) -> str:
    parts: list[str] = []
    current = lines[index][marker_offset:].strip()

    while True:
        continued = current.endswith("\\")
        parts.append(current.removesuffix("\\").strip())
        if not continued or index + 1 >= len(lines):
            break

        index += 1
        current = lines[index].strip()

    return " ".join(part for part in parts if part)


def workflow_step_blocks(job: str, start_line: int) -> list[tuple[int, str]]:
    lines = job.splitlines()
    steps: list[tuple[int, str]] = []

    for index, line in enumerate(lines):
        if not line.startswith("      - "):
            continue

        for end_index in range(index + 1, len(lines)):
            if lines[end_index].startswith("      - "):
                steps.append((start_line + index, "\n".join(lines[index:end_index])))
                break
        else:
            steps.append((start_line + index, "\n".join(lines[index:])))

    return steps


def step_has_if_guard(step: str) -> bool:
    return any(
        line.startswith("      - if:") or line.startswith("        if:")
        for line in step.splitlines()
    )


def ci_docker_build_commands(ci: str, start_line: int = 1) -> list[tuple[int, list[str]]]:
    commands: list[tuple[int, list[str]]] = []
    lines = ci.splitlines()

    for offset, line in enumerate(lines):
        marker_offset = line.find("docker buildx build")
        if marker_offset == -1:
            continue

        command = shell_continuation_command(lines, offset, marker_offset)
        try:
            words = shlex.split(command)
        except ValueError:
            words = command.split()
        commands.append((start_line + offset, words))

    return commands


def command_has_target(words: list[str], target: str) -> bool:
    for index, word in enumerate(words):
        if word == "--target" and index + 1 < len(words) and words[index + 1] == target:
            return True
        if word == f"--target={target}":
            return True
    return False


def command_pushes_image(words: list[str]) -> bool:
    return (
        "--push" in words
        or any(word.startswith("--push=") for word in words)
        or any("push=true" in word for word in words)
        or any("type=registry" in word for word in words)
    )


def assert_ci_runs_pr_docker_build_without_push() -> None:
    ci = read_repo_file(CI_WORKFLOW)
    pull_request_line, pull_request_block = workflow_mapping_block(
        ci, indent=2, key="pull_request"
    )
    pull_request_header = pull_request_block.splitlines()[0]
    path_filter_lines = [
        f"{CI_WORKFLOW}:{line_number}: {line.strip()}"
        for line_number, line in enumerate(
            pull_request_block.splitlines(), start=pull_request_line
        )
        if PATH_FILTER_RE.match(line)
    ]
    if "paths:" in pull_request_header or "paths-ignore:" in pull_request_header:
        path_filter_lines.append(
            f"{CI_WORKFLOW}:{pull_request_line}: {pull_request_header.strip()}"
        )
    if path_filter_lines:
        raise SystemExit(
            "PR CI must not path-filter Docker-sensitive changes:\n"
            + "\n".join(path_filter_lines)
        )

    assert (
        "python3 tests/dockerfile_policy.py" in ci
    ), "PR CI must run Dockerfile policy checks"

    docker_job_line, docker_job = workflow_job_block(ci, "docker-image")
    assert (
        "needs: workflow-policy" in docker_job
    ), "PR Docker build job must depend on workflow-policy"
    assert (
        "docker/setup-buildx-action@" in docker_job
    ), "PR CI must set up Docker Buildx before Docker image builds"
    assert (
        "docker/login-action@" not in docker_job
    ), "PR Docker builds must not log in to a registry"
    assert (
        "docker/build-push-action@" not in docker_job
    ), "PR Docker builds must use docker buildx build directly, without push actions"
    assert "docker push" not in docker_job, "PR Docker builds must not push images"

    guarded_lines = [
        f"{CI_WORKFLOW}:{line_number}: {line.strip()}"
        for line_number, line in enumerate(docker_job.splitlines(), start=docker_job_line)
        if line.startswith("    if:")
    ]
    if guarded_lines:
        raise SystemExit(
            "PR Docker build job must not be guarded away from pull_request events:\n"
            + "\n".join(guarded_lines)
        )

    guarded_build_steps = [
        f"{CI_WORKFLOW}:{line_number}: {step.splitlines()[0].strip()}"
        for line_number, step in workflow_step_blocks(docker_job, docker_job_line)
        if "docker buildx build" in step and step_has_if_guard(step)
    ]
    if guarded_build_steps:
        raise SystemExit(
            "PR Docker build steps must not be guarded away from pull_request events:\n"
            + "\n".join(guarded_build_steps)
        )

    commands = ci_docker_build_commands(docker_job, start_line=docker_job_line)
    assert commands, "PR CI must run docker buildx build"

    pushing_commands = [
        f"{CI_WORKFLOW}:{line_number}: {' '.join(words)}"
        for line_number, words in commands
        if command_pushes_image(words)
    ]
    if pushing_commands:
        raise SystemExit(
            "PR Docker builds must not push images:\n" + "\n".join(pushing_commands)
        )

    assert any(
        command_has_target(words, "builder") for _, words in commands
    ), "PR CI must build the Docker builder target"
    assert any(
        command_has_target(words, "production") for _, words in commands
    ), "PR CI must build the final production Docker image"
    assert any(
        "--load" in words and command_has_target(words, "production")
        for _, words in commands
    ), "PR CI must load the production image locally instead of pushing it"


def main() -> None:
    assert_workflow_actions_are_sha_pinned()
    assert_ci_runs_pr_docker_build_without_push()
    print("Workflow action and PR Docker build policy checks passed.")


if __name__ == "__main__":
    main()
