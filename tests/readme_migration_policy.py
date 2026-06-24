#!/usr/bin/env python3
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read_repo_file(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def section_between(contents: str, start: str, end: str) -> str:
    start_index = contents.index(start)
    end_index = contents.index(end, start_index)
    return contents[start_index:end_index]


def fenced_code_blocks(markdown: str) -> list[str]:
    blocks: list[str] = []
    lines = markdown.splitlines()
    in_block = False
    current: list[str] = []

    for line in lines:
        if line.startswith("```"):
            if in_block:
                blocks.append("\n".join(current))
                current = []
            in_block = not in_block
            continue

        if in_block:
            current.append(line)

    return blocks


def assert_runner_is_canonical(database_setup: str) -> None:
    assert (
        "`discord-transcript migrate`" in database_setup
    ), "README database setup must present discord-transcript migrate as canonical"
    assert (
        "schema_migrations" in database_setup
    ), "README must explain the schema_migrations recording contract"


def assert_raw_psql_file_loop_is_not_documented(database_setup: str) -> None:
    dangerous_snippets = (
        "find migrations -maxdepth 1",
        "psql -f migrations/*.sql",
        'psql -d discord_transcript -f "$f"',
    )
    for snippet in dangerous_snippets:
        assert (
            snippet not in database_setup
        ), f"README must not document raw SQL migration loop: {snippet}"

    for block in fenced_code_blocks(database_setup):
        assert not (
            "psql" in block and "migrations" in block and "schema_migrations" not in block
        ), "README code blocks must not apply migration files without schema_migrations"


def main() -> None:
    readme = read_repo_file("README.md")
    database_setup = section_between(
        readme,
        "### 2. データベースのセットアップ",
        "### 3. 環境変数の設定",
    )
    assert_runner_is_canonical(database_setup)
    assert_raw_psql_file_loop_is_not_documented(database_setup)
    print("README migration policy checks passed.")


if __name__ == "__main__":
    main()
