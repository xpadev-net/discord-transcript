# discord-transcript

Discord のボイスチャンネルを録音し、whisper.cpp で文字起こし、Claude で要約を生成して結果をテキストチャンネルに投稿する Bot です。

## 前提条件

| ツール | バージョン |
|--------|-----------|
| Rust (stable) | Edition 2024 |
| PostgreSQL | 14 以上推奨 |
| [whisper.cpp](https://github.com/ggerganov/whisper.cpp) server | `/inference` エンドポイントが使えること |
| [Claude CLI](https://docs.anthropic.com/) | `claude --model <model> -p` でプロンプト実行できること |

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
psql -d discord_transcript -f migrations/0001_mvp_schema.sql
```

### 3. 環境変数の設定

#### 必須

| 変数名 | 説明 | 例 |
|--------|------|-----|
| `DISCORD_TOKEN` | Discord Bot トークン (serenity が `Bot ` プレフィックスを自動付与するため、トークン文字列のみ設定) | `xxxx...` |
| `DISCORD_GUILD_ID` | 対象サーバーの ID | `123456789012345678` |
| `WHISPER_ENDPOINT` | whisper.cpp サーバーの URL | `http://localhost:8080` |
| `CLAUDE_COMMAND` | Claude CLI の実行パス | `/usr/local/bin/claude` |
| `DATABASE_URL` | PostgreSQL 接続文字列 | `postgresql://user:pass@localhost/discord_transcript` |
| `CHUNK_STORAGE_DIR` | 会議ワークスペースのルート (`workspaces/<guild>/<voice>/<meeting>/...`) | `/var/data/chunks` |

#### オプション

| 変数名 | デフォルト | 説明 |
|--------|-----------|------|
| `DATABASE_SSL_MODE` | `disable` | PostgreSQL の SSL モード |
| `SUMMARY_MAX_RETRIES` | `3` | 要約ジョブの最大リトライ回数 |
| `INTEGRATION_RETRY_MAX_ATTEMPTS` | `3` | 外部連携の最大リトライ回数 |
| `INTEGRATION_RETRY_INITIAL_DELAY_MS` | `200` | リトライ初回遅延 (ms) |
| `INTEGRATION_RETRY_BACKOFF_MULTIPLIER` | `2` | 指数バックオフの倍率 |
| `INTEGRATION_RETRY_MAX_DELAY_MS` | `5000` | リトライ最大遅延 (ms) |
| `AUTO_STOP_GRACE_SECONDS` | `60` | ボイスチャネルが空またはボット切断後に自動停止するまでの猶予秒数 |
| `CLAUDE_MODEL` | `haiku` | Claude CLI の `--model` に渡すモデル名 |
| `RUST_LOG` | `info,serenity=warn,songbird=warn` | ログレベル ([tracing-subscriber EnvFilter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) 形式) |

### ワークスペース構造

`CHUNK_STORAGE_DIR` 配下に会議ごとのワークスペースを作成します。

- ルート: `workspaces/<guild_id>/<voice_channel_id>/<meeting_id>/`
- `audio/`: ユーザーごとのチャンクと `mixdown.wav`
- `transcript/`: `transcript_masked.md`（PII マスク済み文字起こし）、`manifest.json`（meeting_id / guild_id / voice_channel_id / language / masking_stats / generated_at）
- `context/`: 将来のドメイン知識ファイル用プレースホルダ
- `summary/`: 将来の要約成果物置き場

Claude 要約はこのワークスペースを作業ディレクトリとして起動し、トランスクリプトはプロンプトに直埋めせず `transcript/transcript_masked.md` を参照します（`transcript/manifest.json` でメタデータを共有）。

### 4. ビルド

```bash
cargo build --release
```

ビルド成果物は `target/release/discord-transcript` に生成されます。

### 5. 起動

```bash
# 環境変数を設定済みの状態で
cargo run --release

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
cargo test --workspace --all-targets --all-features

# 特定のテストファイルを実行
cargo test --test mvp_core
```

テストではインメモリのストア・スタブクライアントを使用するため、外部サービスは不要です。

## CI

GitHub Actions で push / PR 時に以下が自動実行されます。

- `cargo fmt --all -- --check` (フォーマットチェック)
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` (Lint)
- `cargo test --workspace --all-targets --all-features` (テスト)

## デプロイ

### バイナリデプロイ

```bash
cargo build --release
# target/release/discord-transcript をサーバーに配置
```

実行環境では以下を確認してください。

- 全ての必須環境変数が設定されていること
- PostgreSQL に接続可能で、マイグレーションが適用済みであること
- whisper.cpp サーバーが起動していること
- Claude CLI がインストール・認証済みであること
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
  lib.rs               # モジュールエクスポート
  config.rs            # 環境変数からの設定読み込み
  domain.rs            # コア型定義 (MeetingStatus, StopReason, JobType 等)
  runtime.rs           # Bot ランタイム・イベントハンドリング
  command.rs           # スラッシュコマンド実装
  bot.rs               # Bot コマンドサービスレイヤー
  authz.rs             # 権限チェック
  recorder.rs          # 音声録音管理
  recording_session.rs # 録音セッション状態
  receiver.rs          # ボイスフレーム受信
  audio.rs             # 音声処理ユーティリティ
  meeting_audio.rs     # 録音チャンク読み出し・話者別 WAV 生成
  songbird_adapter.rs  # Songbird ボイスクライアントアダプタ
  asr.rs               # whisper.cpp クライアントインターフェース
  integrations.rs      # 外部連携クライアント (Whisper, Claude CLI)
  summary.rs           # 要約生成パイプライン
  transcript.rs        # 文字起こしの正規化
  privacy.rs           # PII マスキング (メール・電話番号等)
  meeting_flow.rs      # ミーティングライフサイクル制御
  stop.rs              # 録音停止の冪等制御
  auto_stop.rs         # VC 空室時の自動停止
  worker.rs            # バックグラウンドジョブ処理
  queue.rs             # ジョブキュー抽象化
  retry.rs             # リトライポリシー・指数バックオフ
  posting.rs           # Discord メッセージ投稿ユーティリティ
  artifact.rs          # アーティファクト・ストレージ URL 管理
  storage.rs           # ストレージ抽象レイヤー
  storage_fs.rs        # ファイルシステムストレージ実装
  sql.rs               # SQL クエリ定数
  sql_store.rs         # PostgreSQL 実装
  recovery.rs          # Bot 再起動時のリカバリ判定
  recovery_runner.rs   # リカバリ実行
  retention.rs         # データ保持期間 (TTL) 管理
  audit.rs             # 監査ログ
migrations/
  0001_mvp_schema.sql  # DB スキーマ
tests/                 # 統合テスト
.github/workflows/
  ci.yml               # CI 設定
```
