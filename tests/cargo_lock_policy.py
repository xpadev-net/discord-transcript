#!/usr/bin/env python3
import re
import shlex
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
COVERED_FILES = (
    ".github/workflows/ci.yml",
    "Dockerfile",
    "lefthook.yml",
    "README.md",
)
LOCKED_COMMANDS = ("metadata", "clippy", "test", "build", "run")
CARGO_COMMAND_RE = re.compile(rf"\bcargo\s+(?P<subcommand>{'|'.join(LOCKED_COMMANDS)})\b")
SEPARATOR_RE = re.compile(r"\s*(?:&&|\|\||;)\s*")


def read_repo_file(path: str) -> str:
    return (ROOT / path).read_text()


def command_segment(source_line: str, offset: int) -> str:
    command = source_line[offset:]
    command = command.split("`", maxsplit=1)[0]
    return SEPARATOR_RE.split(command, maxsplit=1)[0].strip()


def shell_words(command: str) -> list[str]:
    try:
        return shlex.split(command)
    except ValueError:
        return command.split()


def line_at(contents: str, offset: int) -> tuple[int, str]:
    line = contents.count("\n", 0, offset) + 1
    line_start = contents.rfind("\n", 0, offset) + 1
    line_end = contents.find("\n", offset)
    if line_end == -1:
        line_end = len(contents)
    return line, contents[line_start:line_end]


def line_offset(contents: str, offset: int) -> int:
    return offset - contents.rfind("\n", 0, offset) - 1


def cargo_words(command: str) -> list[str]:
    words = shell_words(command)
    if "--" in words:
        return words[: words.index("--")]
    return words


def assert_covered_cargo_commands_use_locked() -> None:
    errors: list[str] = []

    for path in COVERED_FILES:
        contents = read_repo_file(path)
        for match in CARGO_COMMAND_RE.finditer(contents):
            line, source_line = line_at(contents, match.start())
            if path.endswith((".yml", ".yaml")) and source_line.strip().startswith("- name:"):
                continue

            command = command_segment(source_line, line_offset(contents, match.start()))
            if "--locked" in cargo_words(command):
                continue

            errors.append(f"{path}:{line}: missing --locked in `{command}`")

    assert not errors, "\n".join(errors)


def assert_ci_runs_locked_metadata_check() -> None:
    ci = read_repo_file(".github/workflows/ci.yml")
    for match in CARGO_COMMAND_RE.finditer(ci):
        _, source_line = line_at(ci, match.start())
        if source_line.strip().startswith("- name:"):
            continue

        words = cargo_words(
            command_segment(source_line, line_offset(ci, match.start()))
        )
        if (
            match.group("subcommand") == "metadata"
            and "--locked" in words
            and "--all-features" in words
        ):
            return

    assert False, (
        "CI must independently validate Cargo.lock with "
        "cargo metadata --locked --all-features"
    )


def main() -> None:
    assert_covered_cargo_commands_use_locked()
    assert_ci_runs_locked_metadata_check()
    print("Cargo lockfile policy checks passed.")


if __name__ == "__main__":
    main()
