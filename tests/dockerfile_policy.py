#!/usr/bin/env python3
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read_repo_file(path: str) -> str:
    return (ROOT / path).read_text()


def logical_instructions(contents: str) -> list[str]:
    instructions: list[str] = []
    current = ""

    for raw_line in contents.splitlines():
        line = raw_line.strip()
        if not line:
            continue

        current = f"{current} {line}".strip()
        if current.endswith("\\"):
            current = current[:-1].strip()
            continue

        instructions.append(current)
        current = ""

    if current:
        instructions.append(current)

    return instructions


def dockerfile_stages(contents: str) -> list[tuple[str, list[str]]]:
    stages: list[tuple[str, list[str]]] = []

    for instruction in logical_instructions(contents):
        words = instruction.split()
        if words and words[0].upper() == "FROM":
            stage_name = f"stage-{len(stages)}"
            for index, word in enumerate(words[:-1]):
                if word.upper() == "AS":
                    stage_name = words[index + 1]
                    break
            stages.append((stage_name, []))
        elif stages:
            stages[-1][1].append(instruction)

    return stages


def global_npm_install_specs(instruction: str) -> list[str]:
    if not instruction.split(maxsplit=1)[0].upper() == "RUN":
        return []

    specs: list[str] = []
    for command in instruction.split(";"):
        normalized = command.replace('"', "").replace("'", "")
        words = normalized.split()
        try:
            npm_index = words.index("npm")
        except ValueError:
            continue

        install_index = npm_index + 1
        if install_index >= len(words) or words[install_index] not in {"install", "i"}:
            continue
        if "-g" not in words and "--global" not in words:
            continue

        for word in words[install_index + 1 :]:
            if word in {"-g", "--global", "&&", "||"} or word.startswith("-"):
                continue
            if "=" in word and not word.startswith("@"):
                continue
            specs.append(word)

    return specs


def is_versioned_package_spec(spec: str) -> bool:
    if spec.startswith("@"):
        package, sep, version = spec.rpartition("@")
        return bool(sep and "/" in package and version)

    package, sep, version = spec.rpartition("@")
    return bool(sep and package and version)


def assert_global_npm_installs_are_versioned(dockerfile: str) -> None:
    specs = [
        spec
        for instruction in logical_instructions(dockerfile)
        for spec in global_npm_install_specs(instruction)
    ]
    assert specs, "expected at least one global npm install to keep this policy live"

    for spec in specs:
        assert is_versioned_package_spec(
            spec
        ), f"global npm installs in Dockerfile must include an explicit version: {spec}"


def assert_default_image_is_production_without_claude(dockerfile: str) -> None:
    stages = dockerfile_stages(dockerfile)
    assert stages, "Dockerfile should define stages"

    production_name, production_instructions = stages[-1]
    assert (
        production_name == "production"
    ), "the final Dockerfile stage is the default image and must stay production"

    production_body = "\n".join(production_instructions)
    assert "claude-code" not in production_body, "production image must not install Claude Code"
    assert not any(
        "claude-code" in spec
        for instruction in production_instructions
        for spec in global_npm_install_specs(instruction)
    ), "production image must not install Claude Code through npm"


def assert_unsafe_claude_target_is_pinned_and_verified(dockerfile: str) -> None:
    stages = dict(dockerfile_stages(dockerfile))
    assert "unsafe-claude" in stages, "Dockerfile should keep Claude Code in a named unsafe target"

    body = "\n".join(stages["unsafe-claude"])
    assert (
        "ARG CLAUDE_CODE_VERSION=2.1.178" in body
    ), "unsafe Claude target must pin an exact Claude Code version"
    assert all(
        marker in body
        for marker in (
            "ARG CLAUDE_CODE_INTEGRITY=sha512-",
            "ARG CLAUDE_CODE_LINUX_X64_INTEGRITY=sha512-",
            "ARG CLAUDE_CODE_LINUX_ARM64_INTEGRITY=sha512-",
        )
    ), "unsafe Claude target must pin package integrity values"
    assert (
        "claude --version" in body and "$CLAUDE_CODE_VERSION" in body
    ), "unsafe Claude target must verify the installed claude version"


def assert_compose_uses_unsafe_target() -> None:
    compose = read_repo_file("docker-compose.unsafe-claude.yml")
    assert (
        "target: unsafe-claude" in compose
    ), "unsafe Claude compose override should build the dedicated unsafe target"


def main() -> None:
    dockerfile = read_repo_file("Dockerfile")
    assert_global_npm_installs_are_versioned(dockerfile)
    assert_default_image_is_production_without_claude(dockerfile)
    assert_unsafe_claude_target_is_pinned_and_verified(dockerfile)
    assert_compose_uses_unsafe_target()
    print("Dockerfile policy checks passed.")


if __name__ == "__main__":
    main()
