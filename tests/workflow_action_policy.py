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
JOB_HEADER_RE = re.compile(r"^  (?P<name>[A-Za-z0-9_-]+):\s*(#.*)?$")
DOCKER_BUILD_COMMAND = "docker buildx build"
RUN_PREFIXES = ("run:", "- run:")
# YAML block scalar headers: `|`/`>` plus optional chomping/indentation
# indicators and trailing comments (`run: |`, `run: >-`, `run: |2`,
# `run: | # build image`).
RUN_BLOCK_RE = re.compile(r"^[|>][+\-0-9]*\s*(?:#.*)?$")
# Programs that hand their remaining arguments to another command.
COMMAND_WRAPPERS = {
    "builtin", "chroot", "command", "doas", "env", "exec", "ionice", "nice",
    "nohup", "setsid", "stdbuf", "sudo", "time", "timeout", "watch", "xargs",
}
# Shell keywords that also start a fresh command word.
SHELL_KEYWORDS = {
    "!", "{", "}", "case", "coproc", "do", "done", "elif", "else", "esac",
    "fi", "for", "if", "then", "until", "while",
}
# Wrapper options that take a separate value (e.g. `sudo -u root`), so the
# value is consumed with its flag instead of being read as the command.
WRAPPER_VALUE_FLAGS = {
    "chroot": {"--groups", "--userspec"},
    "doas": {"-C", "-u"},
    "env": {"-C", "-S", "-u", "--chdir", "--split-string", "--unset"},
    "ionice": {"-c", "-n", "-p"},
    "nice": {"-n", "--adjustment"},
    "stdbuf": {"-e", "-i", "-o"},
    # Only options that take a separate value belong here; flags like
    # `sudo -E` or `watch -t` consume nothing, so listing them would hide
    # the real command word behind a supposed option value.
    "sudo": {
        "-C", "-D", "-g", "-h", "-p", "-r", "-R", "-T", "-t", "-U", "-u",
        "--chdir", "--chroot", "--close-from", "--group", "--host",
        "--other-user", "--prompt", "--role", "--type", "--user",
    },
    "time": {"-f", "-o", "--format", "--output"},
    "timeout": {"-k", "-s", "--kill-after", "--signal"},
    "watch": {"-n", "--interval"},
    "xargs": {
        "-d", "-I", "-L", "-n", "-P", "-s", "-J", "--delimiter",
        "--max-args", "--max-chars", "--max-lines", "--max-procs", "--replace",
    },
}
# Programs whose `-c` (or combined short option containing c) argument is
# executed as a command string.
COMMAND_STRING_PROGRAMS = {
    "ash", "bash", "busybox", "dash", "ksh", "mksh", "sh", "su", "yash", "zsh",
}
ENV_ASSIGN_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
WRAPPER_VALUE_TOKEN_RE = re.compile(r"^[0-9]+(\.[0-9]+)?[A-Za-z]*$")
SINGLE_DASH_CLUSTER_RE = re.compile(r"^-[a-zA-Z]+$")
# docker global options that take a separate value before the subcommand.
DOCKER_VALUE_FLAGS = {
    "-H", "--host", "-l", "--log-level", "-c", "--context", "--config",
}
# Characters that always separate shell words, so `docker(push)`-style
# lookalikes cannot hide a push at a word edge.
SHELL_WORD_EDGE_CHARS = ";&|()"


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


def workflow_job_block(contents: str, job_name: str) -> tuple[int, str]:
    lines = contents.splitlines()

    for index, line in enumerate(lines):
        if line == f"  {job_name}:":
            for end_index in range(index + 1, len(lines)):
                if JOB_HEADER_RE.match(lines[end_index]):
                    return index + 1, "\n".join(lines[index:end_index])
            return index + 1, "\n".join(lines[index:])

    raise AssertionError(f"CI workflow must define a {job_name} job")


def workflow_shell_command(line: str) -> str:
    stripped = line.strip()
    for prefix in RUN_PREFIXES:
        if not stripped.startswith(prefix):
            continue
        command = stripped.removeprefix(prefix).strip()
        if command in {"|", ">"}:
            return ""
        return command.strip("\"'")
    return stripped


def shell_continuation_command(lines: list[str], index: int, first_command: str) -> str:
    parts: list[str] = []
    current = first_command.strip()

    while True:
        continued = current.endswith("\\")
        parts.append(current.removesuffix("\\").strip())
        if not continued or index + 1 >= len(lines):
            break

        index += 1
        current = lines[index].strip()

    return " ".join(part for part in parts if part)


def strip_shell_comment(command: str) -> str:
    """Return the command up to the first real POSIX shell comment.

    `#` only opens a comment at the start of a word — a mid-word hash such
    as a URL fragment is literal — and never inside quotes.
    """
    in_single = False
    in_double = False
    index = 0
    while index < len(command):
        char = command[index]
        if in_single:
            in_single = char != "'"
        elif in_double:
            if char == "\\":
                index += 1
            else:
                in_double = char != '"'
        elif char == "'":
            in_single = True
        elif char == '"':
            in_double = True
        elif char == "\\":
            index += 1
        elif char == "#" and (
            index == 0 or command[index - 1] in " \t" + SHELL_WORD_EDGE_CHARS
        ):
            return command[:index]
        index += 1
    return command


def split_shell_segments(command: str) -> list[str]:
    """Split a shell line into command segments at unquoted operators.

    `;`, `&`, `|`, `||`, `&&`, newlines, parentheses, and backticks all end
    a command even without surrounding whitespace, so `.;docker push`
    yields a `docker push` segment.
    """
    segments: list[str] = []
    current: list[str] = []
    in_single = False
    in_double = False
    index = 0
    while index < len(command):
        char = command[index]
        if in_single:
            if char == "'":
                in_single = False
            current.append(char)
        elif in_double:
            if char == "\\":
                current.append(char)
                index += 1
                if index < len(command):
                    current.append(command[index])
            else:
                if char == '"':
                    in_double = False
                current.append(char)
        elif char == "'":
            in_single = True
            current.append(char)
        elif char == '"':
            in_double = True
            current.append(char)
        elif char == "\\":
            current.append(char)
            index += 1
            if index < len(command):
                current.append(command[index])
        elif char in ";&|()\n`":
            segments.append("".join(current))
            current = []
            while index + 1 < len(command) and command[index + 1] in ";&|":
                index += 1
        else:
            current.append(char)
        index += 1
    segments.append("".join(current))
    return segments


def command_word(word: str) -> str:
    return word.rsplit("/", 1)[-1] if word else ""


def command_start_indices(words: list[str]) -> list[int]:
    """Indices where a new command can start inside one shell segment.

    Only words in command position are checked, so `echo docker push`
    stays inert while `sudo`, `env`, `timeout 5`, or `VAR=value` prefixes
    still reveal the docker invocation that follows them.
    """
    starts: list[int] = []
    index = 0
    while index < len(words) and ENV_ASSIGN_RE.match(words[index]):
        index += 1
    if index < len(words):
        starts.append(index)
    while index < len(words):
        head = command_word(words[index])
        if head in SHELL_KEYWORDS:
            index += 1
            if index < len(words):
                starts.append(index)
            continue
        if head not in COMMAND_WRAPPERS:
            index += 1
            continue
        value_flags = WRAPPER_VALUE_FLAGS.get(head, set())
        index += 1
        # Skip wrapper options plus their values and value-like tokens
        # (e.g. `timeout 5`, `nice -n 5`, `env FOO=1`, `sudo -u root`)
        # until the command.
        while index < len(words):
            token = words[index]
            if token in value_flags:
                index += 2
                continue
            if SINGLE_DASH_CLUSTER_RE.match(token):
                # Inside a short-option cluster, the first option that takes
                # a value swallows the REST of the cluster when it has one
                # (`-uroot`) or the next argument when it is last (`-Eu`).
                cluster = token[1:]
                value_at = next(
                    (
                        position
                        for position, char in enumerate(cluster)
                        if f"-{char}" in value_flags
                    ),
                    None,
                )
                if value_at is not None and value_at == len(cluster) - 1:
                    index += 2
                else:
                    index += 1
                continue
            if (
                (token.startswith("-") and token != "-")
                or ENV_ASSIGN_RE.match(token)
                or WRAPPER_VALUE_TOKEN_RE.match(token)
            ):
                index += 1
            else:
                break
        if index < len(words):
            starts.append(index)
    return starts


def words_invoke_docker_push(words: list[str]) -> bool:
    for start in command_start_indices(words):
        if start >= len(words) or command_word(words[start]) != "docker":
            continue
        index = start + 1
        while index < len(words):
            token = words[index]
            if token in DOCKER_VALUE_FLAGS:
                index += 2
            elif token.startswith("-") and token != "-":
                index += 1
            else:
                break
        tail = words[index : index + 2]
        if tail[:1] == ["push"] or tail[:2] == ["image", "push"]:
            return True
    return False


def nested_command_invokes_docker_push(words: list[str], depth: int) -> bool:
    """Scan for command strings passed to shell-style programs.

    `sh -c 'cmd'`, `bash -lc 'cmd'`, `su -c 'cmd'`, and `eval 'cmd'` all
    execute a nested command; combined short-option clusters like `-lc`
    count because the trailing `c` consumes the next argument.
    """
    for index, word in enumerate(words):
        head = command_word(word)
        if head == "eval":
            # eval joins every argument into one command string.
            if command_invokes_docker_push(" ".join(words[index + 1 :]), depth + 1):
                return True
            continue
        if head not in COMMAND_STRING_PROGRAMS:
            continue
        for cursor in range(index + 1, len(words)):
            token = words[cursor]
            if token.startswith("--") or not SINGLE_DASH_CLUSTER_RE.match(token):
                continue
            cluster = token[1:]
            if "c" not in cluster:
                continue
            nested = cluster.split("c", 1)[1]
            if not nested:
                if cursor + 1 >= len(words):
                    continue
                nested = words[cursor + 1]
            if command_invokes_docker_push(nested, depth + 1):
                return True
    return False


def command_invokes_docker_push(command: str, depth: int = 0) -> bool:
    if depth > 4:
        return False
    comment_stripped = strip_shell_comment(command)
    for segment in split_shell_segments(comment_stripped):
        if not segment.strip():
            continue
        try:
            words = shlex.split(segment)
        except ValueError:
            words = segment.split()
        if words_invoke_docker_push(words):
            return True
        # Quoted command strings stay inert unless they are the argument a
        # shell-style program executes, so only those get a nested scan.
        if nested_command_invokes_docker_push(words, depth):
            return True
    return False


def docker_push_command_lines(job: str, start_line: int) -> list[str]:
    lines: list[str] = []
    raw_lines = job.splitlines()
    run_block_indent: int | None = None

    for offset, line in enumerate(raw_lines):
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())
        if run_block_indent is not None:
            if not stripped:
                continue  # blank lines stay inside a block scalar
            if indent > run_block_indent:
                if command_invokes_docker_push(stripped):
                    lines.append(f"{CI_WORKFLOW}:{start_line + offset}: {stripped}")
                continue
            run_block_indent = None
        for prefix in RUN_PREFIXES:
            if not stripped.startswith(prefix):
                continue
            command = stripped.removeprefix(prefix).strip().strip("\"'")
            if not command or RUN_BLOCK_RE.match(command):
                run_block_indent = indent
            elif command_invokes_docker_push(command):
                lines.append(f"{CI_WORKFLOW}:{start_line + offset}: {stripped}")
            break

    return lines


def ci_docker_build_commands(ci: str, start_line: int = 1) -> list[tuple[int, list[str]]]:
    commands: list[tuple[int, list[str]]] = []
    lines = ci.splitlines()

    for offset, line in enumerate(lines):
        first_command = workflow_shell_command(line)
        if not first_command.startswith(DOCKER_BUILD_COMMAND):
            continue

        command = shell_continuation_command(lines, offset, first_command)
        try:
            words = shlex.split(command)
        except ValueError:
            words = command.split()
        commands.append((start_line + offset, words))

    return commands


def command_pushes_image(words: list[str]) -> bool:
    return (
        "--push" in words
        or any(word.startswith("--push=") for word in words)
        or any("push=true" in word for word in words)
        or any(
            ("--output" in word or "--cache-to" in word) and "type=registry" in word
            for word in words
        )
        or any(
            word in {"--output", "--cache-to"}
            and index + 1 < len(words)
            and "type=registry" in words[index + 1]
            for index, word in enumerate(words)
        )
    )


def assert_pr_ci_never_pushes_docker_images() -> None:
    ci = read_repo_file(CI_WORKFLOW)

    assert (
        "python3 tests/dockerfile_policy.py" in ci
    ), "PR CI must run Dockerfile policy checks"

    assert (
        "docker/login-action@" not in ci
    ), "PR CI must not log in to a container registry"
    assert (
        "docker/build-push-action@" not in ci
    ), "PR CI must not use push-capable Docker build actions"

    push_commands = docker_push_command_lines(ci, 1)
    if push_commands:
        raise SystemExit(
            "PR CI must not push Docker images:\n" + "\n".join(push_commands)
        )

    pushing_builds = [
        f"{CI_WORKFLOW}:{line_number}: {' '.join(words)}"
        for line_number, words in ci_docker_build_commands(ci, 1)
        if command_pushes_image(words)
    ]
    if pushing_builds:
        raise SystemExit(
            "PR CI must not push Docker images:\n" + "\n".join(pushing_builds)
        )


def self_test_command_invokes_docker_push() -> None:
    invokes_cases = [
        "docker push example/image:latest",
        "docker image push example/image:latest",
        "docker buildx build --target production --load . && docker push example/image:latest",
        "docker buildx build --load . || docker push example/image:latest",
        "docker buildx build --load .; docker push example/image:latest",
        "docker buildx build --load . | docker push example/image:latest",
        "(docker push example/image:latest)",
        "sudo docker push example/image:latest",
        "xargs docker push",
        "sh -c 'docker push example/image:latest'",
        "sh -c 'docker buildx build --load . && docker push example/image:latest'",
        "docker buildx build --build-arg URL=https://example.test/#fragment --load . && docker push example/image:latest",
        "docker buildx build --load . && docker push example/image:latest # publish",
        "docker buildx build --target production --load .;docker push example/image:latest",
        "docker buildx build --load . ;docker push example/image:latest",
        "bash -lc 'docker push example/image:latest'",
        "bash -ec 'docker push example/image:latest'",
        "su -c 'docker push example/image:latest'",
        "eval 'docker push example/image:latest'",
        "time docker push example/image:latest",
        "timeout 60 docker push example/image:latest",
        "env FOO=1 docker push example/image:latest",
        "nice -n 5 docker push example/image:latest",
        "FOO=bar docker push example/image:latest",
        "$(docker push example/image:latest)",
        "`docker push example/image:latest`",
        "/usr/bin/docker push example/image:latest",
        "docker --log-level debug push example/image:latest",
        "docker image push --all-tags example/image",
        "sudo -u root docker push example/image:latest",
        "sudo -E --preserve-env docker push example/image:latest",
        "env -u HOME docker push example/image:latest",
        "nice -n 5 docker push example/image:latest",
        "timeout -k 2 60 docker push example/image:latest",
        "xargs -I img docker push img",
        "if true; then docker push example/image:latest; fi",
        "sudo -E docker push example/image:latest",
        "sudo -Eu root docker push example/image:latest",
        "sudo -uroot docker push example/image:latest",
        "sudo -R / docker push example/image:latest",
        "watch -t docker push example/image:latest",
        "while read -r tag; do docker push example/image:$tag; done",
        "eval docker push example/image:latest",
        "eval docker buildx build --load . && docker push example/image:latest",
    ]
    inert_cases = [
        "docker buildx build --target production --load .",
        "echo 'docker push example/image:latest'",
        'echo "docker push example/image:latest"',
        "echo docker push example/image:latest",
        "docker push-notify example/image:latest",
        "docker image prune --force",
        "echo done # docker push example/image:latest",
        "echo done; # docker push example/image:latest",
        "docker buildx build --build-arg URL=https://example.test/#fragment --load .",
        "bash -lc 'echo docker push example/image:latest'",
        "docker run alpine sh -c 'echo docker push example/image:latest'",
        "grep -c docker /var/log/build.log",
        "timeout 5 echo docker push example/image:latest",
        "sudo -u root echo docker push example/image:latest",
        "eval echo docker push example/image:latest",
        "if true; then echo docker push example/image:latest; fi",
        "sudo -E echo docker push example/image:latest",
        "watch -t echo docker push example/image:latest",
    ]
    for command in invokes_cases:
        assert command_invokes_docker_push(command), (
            f"expected docker push to be detected: {command}"
        )
    for command in inert_cases:
        assert not command_invokes_docker_push(command), (
            f"expected no docker push: {command}"
        )


def self_test_docker_push_command_lines() -> None:
    sneaky_job = """
      - name: build and push
        run: | # build image
          docker buildx build --load .

          docker push example/image:latest
      - name: indented push
        run: |2
          docker push example/image:latest
    """
    detected = docker_push_command_lines(sneaky_job, 1)
    assert len(detected) == 2, (
        f"expected both pushes to be detected despite comments/blank lines: {detected}"
    )

    safe_job = """
      - name: build
        run: |
          docker buildx build --target production --load .
      - name: docs
        run: echo 'docker push example/image:latest'
      - name: notes
        run: >
          explains how docker push works
    """
    assert not docker_push_command_lines(safe_job, 1), (
        "expected quoted/displayed text and non-push lines to stay inert"
    )


def assert_ci_runs_postgres_sql_contract_smoke() -> None:
    ci = read_repo_file(CI_WORKFLOW)
    _, rust_job = workflow_job_block(ci, "rust")

    assert "services:" in rust_job, "Rust CI must define services for Postgres smoke"
    assert "postgres:" in rust_job, "Rust CI must define a postgres service"
    assert "image: postgres:" in rust_job, "Postgres smoke must use a postgres service image"
    assert (
        "--health-cmd" in rust_job and "pg_isready" in rust_job
    ), "Postgres service must have a readiness health check"
    assert (
        "DISCORD_TRANSCRIPT_TEST_DATABASE_URL:" in rust_job
    ), "Postgres smoke must set DISCORD_TRANSCRIPT_TEST_DATABASE_URL"
    assert (
        "cargo test --locked --test postgres_sql_contract_smoke" in rust_job
    ), "Rust CI must run the Postgres SQL contract smoke test with --locked"


def main() -> None:
    self_test_command_invokes_docker_push()
    self_test_docker_push_command_lines()
    assert_workflow_actions_are_sha_pinned()
    assert_pr_ci_never_pushes_docker_images()
    assert_ci_runs_postgres_sql_contract_smoke()
    print("Workflow action and PR Docker policy checks passed.")


if __name__ == "__main__":
    main()
