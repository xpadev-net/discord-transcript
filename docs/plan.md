以下は **MVP確定版実装計画** です。初版は「1会議を確実に録って返す」ことを唯一の目標にし、スコープを明示的に圧縮します。

## 0. MVPスコープ（確定）

初版（Phase 1）に含めるもの:
- `/record-start`（開始）
- `/record-stop`（手動停止）
- VC空時の自動停止（grace period + cancel）
- Songbird受信による録音（WAVチャンク）
- whisper.cpp server 連携によるASR
- `claude -p` による**単段要約**
- 開始コマンド実行チャンネル（`report_channel_id`）への投稿
- 停止処理のDB条件更新による厳密冪等化
- Bot再起動時の復旧（`recording`/`stopping` 回収）

初版（Phase 1）から除外するもの:
- `api/admin`（管理API）
- 二段要約（チャンク要約 + 統合要約）
- 再要約UI
- 高度な運用画面・ダッシュボード

これにより、初版の実行単位は `discord-recorder`、`asr-worker`、`summary-worker`、Postgres、Object Storage の最小構成に限定する。

## 1. 全体方針

Rust製 Discord Bot（serenity + Songbird）が slash command で録音を制御し、VC空時に自動停止、停止後にASRと要約を実行し、結果を開始チャンネルへ返す。

- Discord Bot: Rust + serenity + Songbird
- ASR: whisper.cpp server (`/inference`)
- 要約: `claude -p`
- 永続化: Postgres + Object Storage

重要な設計原則:
- 投稿先は常に `report_channel_id`（開始コマンド実行チャンネル）
- 停止経路（手動/自動/異常）を `stop_meeting()` に集約
- 停止開始はDBの条件付き更新で一意化（CAS相当）
- 再起動時は中途状態を必ず回収し、ゾンビ会議を残さない

## 2. 仕様（MVP確定）

### 2.1 コマンド

- `/record-start`
  - 実行者のVC参加を検証
  - 権限（VC接続/送信/投稿）確認
  - `meeting`作成（`scheduled`）
  - `report_channel_id = interaction.channel_id` 保存
  - VC join + 受信ハンドラ登録
  - `recording` に遷移
  - ephemeralで開始通知

- `/record-stop`
  - guild内の進行中meeting停止を要求
  - 停止処理は冪等
  - すでに `stopping` 以降なら成功扱い

### 2.2 自動停止

- `VoiceStateUpdateEvent` で対象VCの非Bot人数を追跡
- 0人になったらgrace periodタイマー開始（15〜30秒）
- 戻り参加があれば `CancellationToken` でキャンセル
- 期限後も0人なら `stop_reason=auto_empty` で停止要求

### 2.3 投稿

- interaction返信ではなく通常メッセージとして `report_channel_id` に投稿
- 手動停止/自動停止/異常停止いずれも同じ投稿経路
- ASR/要約失敗時も開始チャンネルへ必ず通知（silent failure禁止）

## 3. 停止処理の厳密冪等性（必須）

停止開始はアプリ内フラグではなくDB条件更新で制御する。

例:

```sql
UPDATE meetings
SET
  status = 'stopping',
  stop_reason = $1,
  stopped_at = NOW()
WHERE id = $2
  AND status = 'recording';
```

判定:
- 更新件数 = 1: この実行が停止処理オーナー。後続（flush/ASR enqueue）へ進む
- 更新件数 = 0: 他経路が先行済み。成功扱いで終了

これにより、手動停止・自動停止・`ClientDisconnect`・一時的な重複イベントが競合しても二重終了を防止できる。

## 4. Bot再起動時の復旧手順（必須）

起動フックで `recording` と `stopping` のmeetingをスキャンして回収する。

復旧フロー:
1. `status IN ('recording', 'stopping')` を取得
2. VC接続状態を確認
3. VC未接続の場合:
   - 録音ファイルあり: `stopping` へ遷移済みならASR再投入、未遷移なら `client_disconnect` で停止確定後ASRへ
   - 録音ファイルなし: `failed` へ遷移（`error_message` 記録）
4. `stopping` かつASR未投入ならジョブ投入を再試行
5. すべての回収結果をログ/メトリクスへ記録

復旧の目的は「録音中のまま残るゾンビmeetingをゼロにする」こと。

## 5. 投稿設計（Discord制約対応）

制約:
- 通常メッセージは1件2000文字まで
- 添付ファイル上限（非Nitro想定）を超える可能性あり

固定方針:
1. 本文は要約・決定事項・ToDo・未決事項を優先（全文貼り付けを避ける）
2. 長文は複数メッセージに分割
3. transcriptは圧縮テキスト添付またはObject Storage期限付きリンク
4. 添付上限超過時は添付せずリンクのみ投稿

これにより投稿失敗率を下げ、閲覧性を維持する。

## 6. 保持期間・権限・PII（初版から適用）

### 6.1 保持期間（TTL）

- raw音声: 3〜7日
- transcript: 30〜90日
- summary: 明示削除まで保持（運用で上限を後付け可）

TTL削除は定期ジョブで実行し、削除ログを残す。

### 6.2 権限

ロール:
- 開始者
- サーバー管理者
- Bot管理者

操作権限（最小）:
- 閲覧（summary/transcriptリンク）
- 再処理（MVPでは内部運用のみ。外部UIはPhase 2）
- 削除（summary/transcript/raw）

### 6.3 PII対策

summary前の正規化で、以下のマスキングを実施:
- ユーザー名/メンション
- メールアドレスらしき文字列
- 電話番号らしき文字列

完全匿名化ではなく、まずは漏えい抑止を目的とした実用的マスキングを適用する。

## 7. データモデル（MVP最小）

- `meetings(id, guild_id, voice_channel_id, report_channel_id, started_by_user_id, title, status, stop_reason, started_at, stopped_at, meeting_duration_seconds, error_message)`
- `transcripts(id, meeting_id, speaker_id, start_ms, end_ms, text, confidence, is_deleted, created_at)`
- `summaries(id, meeting_id, version, markdown, raw_json, created_at)`
- `jobs(id, meeting_id, job_type, status, retry_count, error_message, created_at, updated_at)`
- `artifacts(id, meeting_id, kind, storage_url, size_bytes, expires_at, created_at)`（添付/リンク管理）

状態:
- meeting: `scheduled -> recording -> stopping -> transcribing -> summarizing -> posted`
- 例外: `failed`, `aborted`
- job: `queued`, `running`, `failed`, `done`

## 8. 実装順（確定）

1. slash command登録（`/record-start`, `/record-stop`）
2. meeting作成・保存（`report_channel_id` 含む）
3. `VoiceStateUpdateEvent` による人数監視
4. Songbird録音（受信イベント/ユーザー別バッファ/WAVチャンク）
5. DB条件更新によるstop冪等化（CAS）
6. Bot再起動復旧（`recording`/`stopping` 回収）
7. whisper.cpp連携（ASR）
8. `claude -p` 単段要約
9. 投稿分割・添付/リンク分岐
10. TTL削除ジョブ（raw/transcript）

この順番で「壊れず止まる」を先に固め、その後にASR・要約・投稿品質を載せる。

## 9. GitHub Issues貼り付け用タスクリスト

### Epic A: Command & Meeting Core

- A-1 `/record-start` と `/record-stop` のguild command登録
  - Done条件: 両コマンドが該当guildで実行可能
- A-2 `/record-start` 入力/権限検証 + `meetings` 作成
  - Done条件: `report_channel_id` を含むmeeting行が保存される
- A-3 `/record-stop` 停止要求導線
  - Done条件: 実行時に停止処理へ遷移できる
- A-4 interaction返信をephemeral統一
  - Done条件: 操作系レスポンスが公開チャンネルへ漏れない

### Epic B: Recording Lifecycle

- B-1 Songbird `receive` 有効化 + 受信イベント購読
  - Done条件: 音声受信イベントがログ確認できる
- B-2 ユーザー別バッファ + WAVチャンクflush（15〜30秒）
  - Done条件: Object Storageまたはローカルにチャンク保存される
- B-3 `VoiceStateUpdateEvent` 人数監視
  - Done条件: 対象VCの非Bot人数を追跡できる
- B-4 grace period + `CancellationToken` キャンセル
  - Done条件: 一時離脱復帰で自動停止が誤発火しない
- B-5 stop reason付与（manual/auto_empty/client_disconnect/error）
  - Done条件: `meetings.stop_reason` に正しく保存される

### Epic C: Strict Stop Idempotency

- C-1 DB条件更新で `recording -> stopping` を一意化
  - Done条件: 更新件数1のみ停止オーナーになる
- C-2 手動/自動/異常停止を `stop_meeting()` へ集約
  - Done条件: 停止経路が単一実装になっている
- C-3 競合停止テスト（連打・同時イベント）
  - Done条件: 二重flush/二重ASR投入が発生しない

### Epic D: Restart Recovery

- D-1 起動時スキャン（`recording`/`stopping`）
  - Done条件: 対象meeting一覧を回収対象として取得可能
- D-2 録音ファイル有無で復旧分岐
  - Done条件: `ASR再投入` or `failed遷移` が実行される
- D-3 VC未接続時の `client_disconnect` 回収
  - Done条件: ゾンビ `recording` が残らない
- D-4 回収処理の監査ログ出力
  - Done条件: 復旧結果が追跡可能

### Epic E: ASR & Summary

- E-1 whisper.cpp `/inference` 連携
  - Done条件: WAVからtranscriptが取得できる
- E-2 transcript正規化（空文字除去/連結/ノイズ印）
  - Done条件: summary入力として安定した形式になる
- E-3 `claude -p` 単段要約
  - Done条件: 要約/決定事項/ToDo/未決事項を生成できる
- E-4 失敗時リトライ可能な `jobs` 状態管理
  - Done条件: failedジョブの再実行が可能

### Epic F: Posting & Artifact Strategy

- F-1 `report_channel_id` への投稿実装
  - Done条件: 成功時に開始チャンネルへ投稿される
- F-2 2000文字超過時のメッセージ分割
  - Done条件: 投稿エラーなしで全文送信できる
- F-3 transcript添付/期限付きリンクの分岐
  - Done条件: サイズ条件に応じて自動切替できる
- F-4 添付上限超過時のリンクフォールバック
  - Done条件: 大容量でも失敗通知で終わらない
- F-5 ASR/要約失敗時の障害通知投稿
  - Done条件: silent failureが発生しない

### Epic G: Retention, Access, Privacy

- G-1 raw/transcript/summary のTTLルール実装
  - Done条件: 定期ジョブで期限切れデータが削除される
- G-2 役割別アクセス制御（開始者/管理者/Bot管理者）
  - Done条件: 非許可ユーザーが削除/再処理できない
- G-3 summary前PIIマスキング
  - Done条件: メール/電話/メンション等がマスクされる
- G-4 削除・アクセスの監査ログ
  - Done条件: 重要操作が追跡可能

## 10. MVP完了条件（再定義）

MVP完了の判定は次の4点:
- 録れる: `/record-start` で録音開始し、音声が保存される
- 止まる: 手動停止・VC空自動停止が競合時も二重終了しない
- 復旧できる: Bot再起動後に中途meetingを回収し、ゾンビを残さない
- 返せる: 開始チャンネルへ要約結果を制約内で確実に投稿できる

この4条件を満たした時点でMVPを完了とする。
