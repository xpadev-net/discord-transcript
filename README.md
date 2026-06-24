# discord-transcript

Discord のボイスチャンネルを録音し、whisper.cpp で文字起こし、CLI 経由の LLM（Claude Code / Claude CLI、[Cursor Agent CLI](https://cursor.com/docs/cli/)、[OpenCode](https://opencode.ai/docs/cli/) など）で要約を生成して結果をテキストチャンネルに投稿する Bot です。

## 前提条件

| ツール | バージョン |
|--------|-----------|
| Rust (stable) | Edition 2024 |
| PostgreSQL | 14 以上推奨 |
| [whisper.cpp](https://github.com/ggerganov/whisper.cpp) server | `/inference` エンドポイントが使えること |
| 要約用 CLI | `SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS=true` と `SUMMARY_UNSAFE_AGENT_HARNESS_PROFILE=local-dev` を明示したローカル検証用途のみ（既定 harness は Claude、production Docker image には同梱しません） |

## 環境構築

### 1. リポジトリのクローン

```bash
git clone https://github.com/xpadev-net/discord-transcript.git
cd discord-transcript
```

### 2. データベースのセットアップ

PostgreSQL にデータベースを作成し、マイグレーションを適用します。

```bash
createdb discord_transcript
migration_files=$(find migrations -maxdepth 1 -name "*.sql" | sort)
[ -n "$migration_files" ] || { echo "No migration files found in migrations/"; exit 1; }
printf '%s\n' "$migration_files" | while IFS= read -r f; do
  psql -d discord_transcript -f "$f" || exit 1
done
```

### 3. 環境変数の設定

#### 必須

| 変数名 | 説明 | 例 |
|--------|------|-----|
| `DISCORD_TOKEN` | Discord Bot トークン (serenity が `Bot ` プレフィックスを自動付与するため、トークン文字列のみ設定) | `xxxx...` |
| `DISCORD_GUILD_ID` | 対象サーバーの ID | `123456789012345678` |
| `WHISPER_ENDPOINT` | whisper.cpp サーバーの URL | `http://localhost:8080` |
| `CLAUDE_COMMAND` | **harness が `claude`（既定）**のとき必須。Claude CLI の実行パス（`SUMMARY_COMMAND` 未指定時のみ使用） | `/usr/local/bin/claude` |
| `DATABASE_URL` | PostgreSQL 接続文字列 | `postgresql://user:pass@localhost/discord_transcript` |
| `CHUNK_STORAGE_DIR` | 会議ワークスペースのルート (`workspaces/<guild>/<voice>/<meeting>/...`) | `/var/data/chunks` |

#### オプション

| 変数名 | デフォルト | 説明 |
|--------|-----------|------|
| `APP_ROLE` | `all` | 起動ロール。`all` は従来互換、`web-bot` は Web/API と Discord gateway のみ、`worker` は standalone summary worker のみを起動します。`web-bot` では要約 CLI 設定を要求せず、`worker` では Discord gateway credentials を要求しません。 |
| `DATABASE_SSL_MODE` | `disable` | PostgreSQL の SSL モード。現時点では TLS 接続を実装していないため `disable` のみ対応し、`require` など他の値では起動を拒否します。 |
| `SUMMARY_MAX_RETRIES` | `3` | 要約ジョブの最大リトライ回数 |
| `INTEGRATION_RETRY_MAX_ATTEMPTS` | `3` | 外部連携の最大リトライ回数 |
| `INTEGRATION_RETRY_INITIAL_DELAY_MS` | `200` | リトライ初回遅延 (ms) |
| `INTEGRATION_RETRY_BACKOFF_MULTIPLIER` | `2` | 指数バックオフの倍率 |
| `INTEGRATION_RETRY_MAX_DELAY_MS` | `5000` | リトライ最大遅延 (ms) |
| `AUTO_STOP_GRACE_SECONDS` | `60` | ボイスチャネルが空またはボット切断後に自動停止するまでの猶予秒数 |
| `CLAUDE_MODEL` | `haiku` | Claude harness 時の `--model`（`SUMMARY_MODEL` 未指定時のフォールバック） |
| `SUMMARY_HARNESS` | `claude` | `claude` / `cursor_agent` / `opencode` |
| `SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS` | `false` | CLI harness へ untrusted transcript を渡す unsafe opt-in。production では既定で拒否します。 |
| `SUMMARY_UNSAFE_AGENT_HARNESS_PROFILE` | 未設定 | `SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS=true` のとき必須。`local` / `local-dev` / `dev` / `development` / `test` / `testing` のみ許可し、production-like な値では起動を拒否します。 |
| `SUMMARY_COMMAND` | 未設定 | 設定時は **どの harness でも最優先**で実行ファイルに使用。非 `claude` harness では **必須**（`CLAUDE_COMMAND` にはフォールバックしない） |
| `SUMMARY_MODEL` | 未設定 | `CLAUDE_MODEL` より優先。**`opencode` では必須**（`provider/model` 形式。例: `anthropic/claude-3-5-haiku-20241022`） |
| `RUST_LOG` | `info,serenity=warn,songbird=warn` | ログレベル ([tracing-subscriber EnvFilter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) 形式) |
| `OPERATIONAL_METRICS_BEARER_TOKEN` | 未設定 | `/metricsz` の Bearer 認証トークン。未設定時は `/metricsz` を無効化します。 |

Docker Compose で worker profile を使ってプロセスを分ける場合は、既定互換の `all` ではなく `APP_ROLE=web-bot docker compose --profile worker up` のように app 側を `web-bot` で起動してください。

> **Note:** 要約 CLI harness はワークスペースや環境へアクセスできるため、既定では起動を拒否します。ローカル検証で使う場合は `SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS=true` と `SUMMARY_UNSAFE_AGENT_HARNESS_PROFILE=local-dev` を明示してください。production-like な profile では unsafe opt-in があっても起動しません。文字起こし補正（LLM によるトランスクリプト整形）は全文プロンプトを扱うため、unsafe opt-in 後も built-in CLI harness では実行しません。

Docker Compose の通常サービスは、要約用 CLI やホストの LLM 認証ディレクトリを含めません。ローカル検証で必要な場合だけ、`LLM_CLAUDE_CONFIG_DIR` / `LLM_OPENCODE_DATA_DIR` / `LLM_CURSOR_CONFIG_DIR` のいずれか 1 つを絶対パスで設定し、対応する unsafe override file を明示的に追加してください。Claude Code のローカル検証では `unsafe-claude` Docker target を使い、固定バージョン・integrity・`claude --version` 検証付きで CLI を追加します。これらの override file は認証ディレクトリを read-only でマウントします。コンテナ内で OpenCode や Cursor を使う場合は、事前にホスト側で `opencode auth login` や Cursor CLI のログインを済ませてください。

```bash
# Claude Code local unsafe harness image
docker compose -f docker-compose.dev.yml -f docker-compose.unsafe-claude.yml up app

# OpenCode / Cursor require a trusted derived image or mounted binary with SUMMARY_COMMAND set.
docker compose -f docker-compose.yml -f docker-compose.unsafe-opencode.yml up app
docker compose -f docker-compose.yml -f docker-compose.unsafe-cursor.yml up app
```

Compose の `app` は Discord voice (songbird) の UDP 通信のため host network を使います。Web UI は既定で `127.0.0.1` に bind します。外部公開する場合は、リバースプロキシなどの公開経路を決めた上で `WEB_BIND_HOST` を明示的に変更してください。`migrate` は compose network 上の `db:5432` に接続するため host network を使いません。

公開 GHCR イメージはネットワークインストーラや registry の最新状態に依存しないため、Claude Code / Cursor Agent CLI / OpenCode を同梱しません。Docker で CLI harness を使う場合は、ローカル検証専用の `unsafe-claude` target を使うか、検証済みの CLI を含む派生イメージを作るか、信頼できる方法でバイナリを配置し、`SUMMARY_COMMAND` に明示的なパスを設定してください。

### ワークスペース構造

`CHUNK_STORAGE_DIR` 配下に会議ごとのワークスペースを作成します。

- ルート: `workspaces/<guild_id>/<voice_channel_id>/<meeting_id>/`
- `audio/`: ユーザーごとのチャンクと `mixdown.wav`
- `transcript/`: `transcript_masked.md`（PII マスク済み文字起こし）、`manifest.json`（meeting_id / guild_id / voice_channel_id / language / masking_stats / generated_at）
- `context/`: 話者 roster、domain knowledge、AI memory、feedback、alias、template manifest など要約用コンテキスト
- `summary/`: 検証済み要約や downstream artifact の保存先

要約・AI memory 抽出用の CLI harness は、実会議ワークスペース全体ではなく、実行ごとに生成される agent workspace を作業ディレクトリとして起動します。agent workspace には許可された入力ファイルだけを exact path で `input/**` にコピーし、成功結果は `output/**` の検証済みファイルだけから読み取ります。stdout は成功時の要約本文や AI memory JSON としては受け付けず、失敗時のサイズ制限・サニタイズ済み診断にだけ使います。

agent workspace の契約:

- 要約入力: `input/transcript/transcript_masked.md`, `input/transcript/manifest.json`, `input/context/manifest.json`, `input/context/speaker_roster.md`, `input/context/domain_knowledge.md`, `input/context/ai_memory.md`, `input/context/person_aliases.md`, `input/context/user_feedback.md`, 任意の `input/context/summary_template.txt`
- 要約出力: `output/summary.md`
- AI memory 抽出入力: 要約入力と同じ context/transcript 一式に加えて、検証済み要約を `input/summary/summary.md` として渡します。
- AI memory 抽出出力: `output/ai_memory_candidates.json`
- 除外: `audio/**`, `debug/**`, 実会議ワークスペースの `summary/**`（AI memory 用に検証済み `summary/summary.md` を個別コピーするときだけ例外）, `.env`, 認証・credential ディレクトリ、リポジトリのソースやテスト、未知のファイル、symlink は agent workspace にコピーしません。

`cursor_agent` harness では agent workspace 内に `.cursor/cli.json` を生成し、materialize された各入力ファイルへの `Read(...)` と実行種別ごとの単一出力ファイルへの `Write(...)` だけを許可する意図を記録します。あわせて `.env`、`debug/**`、`../**`、`input/**` への書き込み、`Shell(*)` を deny します。この設定は Cursor の権限モデル向けの defense-in-depth であり、唯一の境界ではありません。主な境界は、コピー対象を exact path で限定した agent workspace、credential 系環境変数の削除、コマンド実行の制限、`output/**` の検証です。

要約成功時は、`output/summary.md` を regular file / UTF-8 / 非空 / サイズ上限内として検証し、永続化した後に agent workspace を削除します。AI memory 抽出成功時は、`output/ai_memory_candidates.json` を strict JSON schema と候補ルールで検証した後に workspace を削除します。失敗時も stdout を代替結果として使わず、サニタイズ済み診断だけを返して workspace 削除を試みます。要約永続化後の cleanup が失敗した場合はジョブを retry し、残留した `agent/` ディレクトリは retention cleanup の削除対象になります。現時点では failed-run workspace を明示保持する運用設定はありません。

### 4. Git Hooks (lefthook)

[lefthook](https://github.com/evilmartians/lefthook) でコミット前にフォーマットと Lint を自動チェックします。

```bash
brew install lefthook              # macOS
# go install github.com/evilmartians/lefthook/v2@latest  # cross-platform alternative
lefthook install
```

インストール後、`git commit` 時に `cargo fmt --check` と `cargo clippy --locked` が、`git push` 時に `cargo test --locked` が自動実行されます。

### 5. ビルド

```bash
cargo build --release --locked
```

ビルド成果物は `target/release/discord-transcript` に生成されます。

### 6. 起動

```bash
# 環境変数を設定済みの状態で
cargo run --release --locked

# または直接バイナリを実行
./target/release/discord-transcript
```

## Discord Bot の設定

[Discord Developer Portal](https://discord.com/developers/applications) で Bot を作成し、以下を有効にしてください。

### 必要な Intent

- **Guilds** (サーバー情報の取得)
- **Guild Voice States** (ボイスチャンネルの参加・退出検出)

### 必要な Bot Permission

- Connect (ボイスチャンネルへの接続)
- Speak (ボイスチャンネルでの送受信)
- Send Messages (テキストチャンネルへのメッセージ送信)
- View Channel (VC テキストチャットの閲覧)
- Read Message History (会議中 VC チャット履歴の取得)
- Use Slash Commands

### スラッシュコマンド

| コマンド | 説明 |
|----------|------|
| `/record-start` | ユーザーが参加中のボイスチャンネルの録音を開始 |
| `/record-stop` | 録音を停止し、文字起こし・要約を実行 |

## 録音と文字起こしの流れ

- 音声はユーザーごとの WAV チャンクとして保存し、各チャンクには録音開始時刻を埋め込みます。
- Whisper 推論は話者ごとに生成した WAV を入力として実行し、セグメント開始時刻を会議タイムラインに再マッピングして統合します。
- `mixdown.wav` も従来通り生成されるため、再生や API 互換性は維持されます。
- 要約・Web 表示では、Discord から取得した話者プロフィールを `meeting_speakers` テーブルにスナップショットし、ニックネーム→表示名→ユーザー名→ID の優先順位でラベルを付与します（取得に失敗した場合は ID でフォールバック）。
- 録音開始→録音終了→要約開始→要約完了の進捗は、レポートチャンネルに投稿された 1 件の通常メッセージを編集して更新します（要約ページのリンクや失敗理由も同じメッセージに反映されます）。

## テスト

```bash
# 全テスト実行
cargo test --locked --workspace --all-targets --all-features

# 特定のテストファイルを実行
cargo test --locked --test mvp_core
```

テストではインメモリのストア・スタブクライアントを使用するため、外部サービスは不要です。

## CI

GitHub Actions で push / PR 時に以下が自動実行されます。

- `cargo fmt --all -- --check` (フォーマットチェック)
- `cargo metadata --locked --all-features --format-version 1 > /dev/null` (lockfile 検証)
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` (Lint)
- `cargo test --locked --workspace --all-targets --all-features` (テスト)

## デプロイ

### バイナリデプロイ

```bash
cargo build --release --locked
# target/release/discord-transcript をサーバーに配置
```

実行環境では以下を確認してください。

- 全ての必須環境変数が設定されていること
- PostgreSQL に接続可能で、マイグレーションが適用済みであること
- whisper.cpp サーバーが起動していること
- 要約用 CLI（既定なら Claude）がインストール・認証済みであること（production Docker image には同梱しません）
- `CHUNK_STORAGE_DIR` で指定したディレクトリが存在し、書き込み可能であること

### systemd によるサービス化 (例)

```ini
[Unit]
Description=discord-transcript bot
After=network.target postgresql.service

[Service]
Type=simple
EnvironmentFile=/etc/discord-transcript/env
ExecStart=/usr/local/bin/discord-transcript
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now discord-transcript
```

## プロジェクト構成

```text
src/
  main.rs              # エントリーポイント
  lib.rs               # ルートモジュール
  application/         # ユースケース・実行フロー
    runtime.rs         # Bot ランタイム・イベントハンドリング
    command.rs         # スラッシュコマンド実装
    bot.rs             # Bot コマンドサービスレイヤー
    stop.rs            # 録音停止の冪等制御
    auto_stop.rs       # VC 空室時の自動停止
    worker.rs          # バックグラウンドジョブ処理
    summary.rs         # 要約生成パイプライン
  audio/               # 音声受信・録音処理
    wav.rs             # WAV 変換・音声ユーティリティ
    receiver.rs        # ボイスフレーム受信
    recorder.rs        # 音声録音管理
    recording_session.rs
    meeting_audio.rs
    songbird_adapter.rs
  bootstrap/
    config.rs          # 環境変数からの設定読み込み
  domain/              # ドメインルール・型・ポリシー
    model.rs           # コア型定義 (MeetingStatus, StopReason, JobType 等)
    authz.rs
    privacy.rs
    transcript.rs
    retention.rs
    recovery.rs
    audit.rs
  infrastructure/      # 外部I/O・永続化・連携
    sql.rs             # SQL クエリ定数
    sql_store.rs       # PostgreSQL 実装
    storage.rs
    storage_fs.rs
    queue.rs
    integrations.rs
    asr.rs
    retry.rs
    artifact.rs
    workspace.rs
  interfaces/          # 外部向けインターフェース
    web.rs             # Web API
    posting.rs         # Discord メッセージ投稿
migrations/
  *.sql                # DB schema migrations (runtime and Docker apply these)
tests/
  application/         # 統合テスト本体（機能別）
  audio/
  domain/
  infrastructure/
  *.rs                 # Cargo 用エントリ（薄いラッパー）
.github/workflows/
  ci.yml               # CI 設定
```
