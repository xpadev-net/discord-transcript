#!/usr/bin/env python3
import shlex
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read_repo_file(path: str) -> str:
    return (ROOT / path).read_text()


def logical_instructions(contents: str) -> list[str]:
    instructions: list[str] = []
    current = ""

    for raw_line in contents.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
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
    words = shlex.split(instruction)
    index = 1
    while index < len(words):
        if words[index] != "npm":
            index += 1
            continue

        install_index = index + 1
        if install_index >= len(words) or words[install_index] not in {"install", "i"}:
            index += 1
            continue

        command = []
        index = install_index + 1
        while index < len(words) and words[index] not in {"&&", "||", ";"}:
            word = words[index].removesuffix(";")
            if word:
                command.append(word)
            if words[index].endswith(";"):
                break
            index += 1

        if "-g" not in command and "--global" not in command:
            continue

        for word in command:
            if word in {"-g", "--global"} or word.startswith("-"):
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


def assert_native_runtime_image(dockerfile: str) -> None:
    stages = dockerfile_stages(dockerfile)
    assert stages, "Dockerfile should define stages"

    stage_map = dict(stages)
    assert "FROM node:24-bookworm-slim AS frontend" in dockerfile, (
        "frontend builder must keep Node.js"
    )

    production_name, production_instructions = stages[-1]
    assert (
        production_name == "production"
    ), "the final Dockerfile stage is the default image and must stay production"

    production_body = "\n".join(production_instructions)
    runtime = "\n".join(stage_map["runtime-base"])
    production_image = f"{runtime}\n{production_body}".lower()
    production_instructions = stage_map[production_name]
    assert "FROM debian:bookworm-slim AS runtime-base" in dockerfile
    assert all(
        package in runtime
        for package in ("curl", "ca-certificates", "libopus0", "libssl3")
    ), "runtime image must retain required runtime libraries"
    assert "COPY --from=frontend /app/web/dist /app/web/dist" in runtime
    assert "USER app" in runtime and 'ENV STATIC_FILES_DIR=/app/web/dist' in runtime
    assert not any(
        marker in production_image for marker in ("node:", "nodejs", "npm ")
    ), "production image must not add Node.js or npm in any final-stage layer"
    assert "claude-code" not in production_image
    frontend_copies = [
        instruction
        for instruction in stage_map["runtime-base"] + production_instructions
        if instruction.upper().startswith("COPY")
        and "--from=frontend" in instruction.lower()
    ]
    assert frontend_copies == [
        "COPY --from=frontend /app/web/dist /app/web/dist"
    ], "production image may copy only built static assets from the Node frontend stage"
    assert all("unsafe-" not in name for name, _ in stages)


def assert_cli_override_files_removed() -> None:
    for path in (
        "docker-compose.unsafe-claude.yml",
        "docker-compose.unsafe-cursor.yml",
        "docker-compose.unsafe-opencode.yml",
    ):
        assert not (ROOT / path).exists(), f"obsolete CLI override remains: {path}"


def main() -> None:
    dockerfile = read_repo_file("Dockerfile")
    assert_global_npm_installs_are_versioned(dockerfile)
    assert_native_runtime_image(dockerfile)
    assert_cli_override_files_removed()
    print("Dockerfile policy checks passed.")


if __name__ == "__main__":
    main()
